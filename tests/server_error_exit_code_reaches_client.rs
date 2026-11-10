//! A server-side fatal error must reach the client as the server's own
//! `RERR_*` code, carried by `MSG_ERROR_EXIT`.
//!
//! Upstream's `_exit_cleanup()` writes the code it is about to exit with onto
//! the multiplexed stream before the process ends (cleanup.c:242-258), and the
//! peer's `read_a_msg()` turns receipt of that frame into the NORETURN
//! `_exit_cleanup(val, __FILE__, 0 - __LINE__)` (io.c:1892-1930). The client
//! therefore exits with the *server's* code, not with whatever the connection
//! dropping happened to look like locally.
//!
//! The fixture is a push whose server receiver cannot create the destination
//! root: get_local_name() reports `mkdir %s failed` and exits `RERR_FILEIO`
//! whatever the errno (main.c:817-821). It runs before do_recv() forks
//! (main.c:1248), so the line and the trailer name the unforked `Receiver`
//! (rsync.c:987-995). Measured against the rsync 3.5.1 tarball build over a
//! remote shell (3.5.0 gives the same codes):
//!
//! | client | server | protocol | client-observed exit |
//! |--------|--------|----------|----------------------|
//! | rsync  | rsync  | 32       | 11                   |
//! | rsync  | rsync  | 30       | 12                   |
//!
//! The protocol split is upstream's gate. `cleanup.c:244` reads
//! `protocol_version >= 31 || am_receiver`, but `am_receiver` selects the forked
//! receiver child's *sibling* message channel, not the socket; a single-process
//! port must gate on the protocol version alone, which is exactly what the two
//! rows above show on the wire.
//!
//! Every cell runs under the oc client and, when it is installed, the 3.5.1
//! client. The latter answers the frame with `kill_all(SIGUSR1)`
//! (cleanup.c:202-203); the remote shell execs the server, so that kill lands
//! on the server itself, and only a report printed before the frame survives.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use tempfile::TempDir;
use test_support::upstream_compat::{
    UpstreamVersion, locate_upstream_rsync, require_upstream_rsync,
};

const RUN_TIMEOUT: Duration = Duration::from_secs(60);

/// Locates the binary under test. `CARGO_BIN_EXE_oc-rsync` is a compile-time
/// variable, so it must be read with `env!`, never `env::var_os`: at run time it
/// is unset and the lookup would fall through to a stale `target/debug` build.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// The clients every cell runs under: oc, and 3.5.1 when it is installed.
///
/// A 3.5.1 binary that resolves but fails the banner check would silently
/// drop its cells, so it fails the test instead of skipping it.
fn clients() -> Vec<PathBuf> {
    let mut clients = vec![oc_rsync_binary()];
    let located = locate_upstream_rsync(UpstreamVersion::V3_5_1);
    match require_upstream_rsync(UpstreamVersion::V3_5_1) {
        Some(upstream) => clients.push(upstream.binary().to_path_buf()),
        None => assert!(
            located.is_none(),
            "3.5.1 at {located:?} was found but rejected; its cells would not run"
        ),
    }
    for client in &clients {
        eprintln!("server_error_exit_code: client {}", client.display());
    }
    clients
}

/// Effective-UID probe used as a skip gate. Shells out to `id -u` rather than
/// linking `libc::geteuid` so the test stays free of FFI. Root ignores the
/// directory mode the fixture relies on, so the fatal path would never fire.
fn is_root() -> bool {
    match Command::new("id").arg("-u").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .trim()
            .parse::<u32>()
            .map(|uid| uid == 0)
            .unwrap_or(false),
        _ => false,
    }
}

/// Writes an executable `--rsh` shim that drops the host operand and execs the
/// remote command, giving a real two-process transfer with no sshd. The
/// client's child is then the server process itself.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let path = dir.join("rsh.sh");
    fs::write(&path, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    let mut perms = fs::metadata(&path).expect("stat shim").permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&path, perms).expect("chmod shim");
    path
}

fn run_with_timeout(mut command: Command) -> Output {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn client");
    let deadline = Instant::now() + RUN_TIMEOUT;
    loop {
        match child.try_wait().expect("try_wait") {
            Some(_) => break,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                panic!("client did not exit within {RUN_TIMEOUT:?}");
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    child.wait_with_output().expect("collect output")
}

/// Restores the fixture's directory mode however the test leaves.
struct ModeGuard(PathBuf);

impl Drop for ModeGuard {
    fn drop(&mut self) {
        if let Ok(meta) = fs::metadata(&self.0) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = fs::set_permissions(&self.0, perms);
        }
    }
}

/// Pushes from `client` to an oc server into `<dst>/sub/` where `<dst>` is not
/// writable, so the *server* receiver fails to create the destination root.
/// Returns the client's exit code, its combined output and the destination
/// root as the server names it.
fn push_into_unwritable_parent(client: &Path, extra: &[&str]) -> (i32, String, PathBuf) {
    let scratch = TempDir::new().expect("tempdir");
    let src = scratch.path().join("src");
    let dst = scratch.path().join("dst");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::write(src.join("f.txt"), b"payload").expect("write source file");

    let shim = write_rsh_shim(scratch.path());

    let guard = ModeGuard(dst.clone());
    let mut perms = fs::metadata(&dst).expect("stat dst").permissions();
    perms.set_mode(0o555);
    fs::set_permissions(&dst, perms).expect("chmod dst");

    let mut command = Command::new(client);
    command.arg("-e").arg(&shim);
    command.arg(format!("--rsync-path={}", oc_rsync_binary().display()));
    command.args(extra);
    command.arg("-r");
    command.arg(format!("{}/", src.display()));
    command.arg(format!("host:{}/sub/", dst.display()));

    let output = run_with_timeout(command);
    drop(guard);
    let root = dst.join("sub");
    assert!(
        !root.exists(),
        "nothing may be created under the sealed parent"
    );

    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    (output.status.code().unwrap_or(-1), text, root)
}

/// The server's own report: rsyserr()'s line, then log_exit()'s `RERR_FILEIO`
/// trailer naming the unforked receiver, each exactly once and in that order.
///
/// upstream: main.c:817-821, log.c:937-963, rsync.c:987-995
fn assert_server_report(client: &Path, root: &Path, text: &str) {
    assert_mkdir_report(client, &format!("\"{}\"", root.display()), text);
}

/// [`assert_server_report`] for a destination root `full_fname()` renders as
/// `fname`: quoted, and module-relative with ` (in <module>)` inside a daemon
/// module (util1.c:1528).
fn assert_mkdir_report(client: &Path, fname: &str, text: &str) {
    let line = format!("[Receiver] mkdir {fname} failed: Permission denied (13)\n");
    let trailer = "error in file IO (code 11) at ";
    let at_line: Vec<_> = text.match_indices(&line).collect();
    let at_trailer: Vec<_> = text.match_indices(trailer).collect();
    assert_eq!(at_line.len(), 1, "{client:?}: one cause line:\n{text}");
    assert_eq!(at_trailer.len(), 1, "{client:?}: one trailer:\n{text}");
    assert!(
        at_line[0].0 < at_trailer[0].0,
        "{client:?}: the cause precedes the trailer:\n{text}"
    );
    assert!(
        text[at_trailer[0].0..].contains("[Receiver="),
        "{client:?}: the trailer names the unforked receiver:\n{text}"
    );
}

/// The regression. At the negotiated default protocol the server receiver's
/// `RERR_FILEIO` must arrive as the client's own exit code, not the
/// `RERR_FILESELECT` its EACCES would grade, and its report must survive the
/// 3.5.1 client's kill.
///
/// upstream: main.c:817-821; cleanup.c:250 `send_msg_int(MSG_ERROR_EXIT,
/// exit_code)`; io.c:1930 `_exit_cleanup(val, __FILE__, 0 - __LINE__)`.
#[test]
fn server_fatal_exit_code_reaches_the_client() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    for client in clients() {
        let (code, text, root) = push_into_unwritable_parent(&client, &[]);
        assert_eq!(
            code, 11,
            "{client:?}: the client must exit with the server's RERR_FILEIO, not a \
             locally-inferred code; output was:\n{text}"
        );
        assert_server_report(&client, &root, &text);
    }
}

/// The gate's companion: below protocol 31 upstream sends nothing, and the
/// client is left inferring `RERR_STREAMIO` (12) from the closed stream. A
/// blanket "always propagate" change would redden it.
///
/// upstream: cleanup.c:244 `protocol_version >= 31`.
///
/// Measured against rsync 3.5.1 (50 runs each, rsync client and server):
/// protocol 30 exits 12 every time, never 10. The client writes the file list
/// and NDX_FLIST_EOF in one flush (main.c:1398), so it has nothing left to
/// write when the server dies and reads EOF instead of hitting EPIPE. The
/// whine and the server's own report pin that the 12 comes from that EOF.
#[test]
fn protocol_30_keeps_the_stream_io_code() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    for client in clients() {
        let (code, text, root) = push_into_unwritable_parent(&client, &["--protocol=30"]);
        assert_eq!(
            code, 12,
            "{client:?}: protocol 30 is below upstream's MSG_ERROR_EXIT gate, so the \
             client must still report RERR_STREAMIO; output was:\n{text}"
        );
        assert_server_report(&client, &root, &text);
        assert!(
            text.contains("connection unexpectedly closed (0 bytes received so far) [sender]"),
            "{client:?}: the client must have read EOF (io.c:303), not failed a write; \
             output was:\n{text}"
        );
    }
}

/// A transfer that succeeds keeps exiting 0 over the same remote-shell path, so
/// the new producer cannot be firing on the happy path.
#[test]
fn a_successful_remote_shell_push_still_exits_zero() {
    let scratch = TempDir::new().expect("tempdir");
    let src = scratch.path().join("src");
    let dst = scratch.path().join("dst");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&dst).expect("mkdir dst");
    fs::write(src.join("f.txt"), b"payload").expect("write source file");

    let shim = write_rsh_shim(scratch.path());

    let mut command = Command::new(oc_rsync_binary());
    command.arg("-e").arg(&shim);
    command.arg(format!("--rsync-path={}", oc_rsync_binary().display()));
    command.arg("-r");
    command.arg(format!("{}/", src.display()));
    command.arg(format!("host:{}/", dst.display()));

    let output = run_with_timeout(command);
    assert_eq!(
        output.status.code(),
        Some(0),
        "a clean push must still exit 0; stderr was:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        fs::read(dst.join("f.txt")).expect("destination file"),
        b"payload",
        "the payload must land"
    );
}

/// A local copy fails the same way: get_local_name() runs in the client before
/// it forks the local server (main.c:1412), so the report names the unforked
/// `Receiver` and the copy exits `RERR_FILEIO`.
///
/// upstream: main.c:817-821
#[test]
fn local_copy_dest_mkdir_failure_reports_file_io() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    for client in clients() {
        let scratch = TempDir::new().expect("tempdir");
        let src = scratch.path().join("src");
        let dst = scratch.path().join("dst");
        fs::create_dir_all(&src).expect("mkdir src");
        fs::create_dir_all(&dst).expect("mkdir dst");
        fs::write(src.join("f.txt"), b"payload").expect("write source file");

        let guard = ModeGuard(dst.clone());
        let mut perms = fs::metadata(&dst).expect("stat dst").permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&dst, perms).expect("chmod dst");

        let mut command = Command::new(&client);
        command.arg("-r");
        command.arg(format!("{}/", src.display()));
        command.arg(format!("{}/sub/", dst.display()));
        let output = run_with_timeout(command);
        drop(guard);

        let root = dst.join("sub");
        assert!(
            !root.exists(),
            "nothing may be created under the sealed parent"
        );
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            output.status.code(),
            Some(11),
            "{client:?}: a local copy must exit RERR_FILEIO; output was:\n{text}"
        );
        assert_server_report(&client, &root, &text);
    }
}

/// The destination root is created up front only when the first file list
/// holds more than one entry or the destination ends in a slash (main.c:808).
/// Each cell sits on an edge of that count, with no trailing slash:
///
/// - a copy-contents operand whose every child is filtered out lists only its
///   "." entry, so recv_generator creates the root and its mkdir failure is a
///   per-file error (generator.c:1873-1883, exit 23);
/// - `-d src/` lists "." and the children (flist.c:2991), and a file given
///   twice is counted twice (flist.c:3060), so both fail the pre-flight mkdir
///   with `RERR_FILEIO`.
///
/// upstream: main.c:808-821, flist.c:3096 send_extra_file_list()
/// Builds a cell's source operands from the fixture's source directory.
type OperandsFor = fn(&Path) -> Vec<String>;

#[test]
fn local_copy_dest_root_mkdir_follows_file_total() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    let cells: [(&str, OperandsFor, i32); 3] = [
        (
            "every child excluded",
            |src| {
                vec![
                    "-r".into(),
                    "--exclude=f.txt".into(),
                    format!("{}/", src.display()),
                ]
            },
            23,
        ),
        (
            "-d copy-contents",
            |src| vec!["-d".into(), format!("{}/", src.display())],
            11,
        ),
        (
            "file given twice",
            |src| {
                let file = src.join("f.txt").display().to_string();
                vec![file.clone(), file]
            },
            11,
        ),
    ];
    for client in clients() {
        for (label, operands, exit) in cells {
            let scratch = TempDir::new().expect("tempdir");
            let src = scratch.path().join("src");
            let dst = scratch.path().join("dst");
            fs::create_dir_all(&src).expect("mkdir src");
            fs::create_dir_all(&dst).expect("mkdir dst");
            fs::write(src.join("f.txt"), b"payload").expect("write source file");

            let guard = ModeGuard(dst.clone());
            let mut perms = fs::metadata(&dst).expect("stat dst").permissions();
            perms.set_mode(0o555);
            fs::set_permissions(&dst, perms).expect("chmod dst");

            let root = dst.join("new");
            let mut command = Command::new(&client);
            command.args(operands(&src));
            command.arg(&root);
            let output = run_with_timeout(command);
            drop(guard);

            let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&output.stderr));
            assert_eq!(
                output.status.code(),
                Some(exit),
                "{client:?} {label}: output was:\n{text}"
            );
            if exit == 11 {
                assert_server_report(&client, &root, &text);
            } else {
                let line = format!(
                    "recv_generator: mkdir \"{}\" failed: Permission denied (13)\n",
                    root.display()
                );
                assert!(
                    text.contains(&line) && !text.contains("[Receiver] mkdir"),
                    "{client:?} {label}: the root is a per-file mkdir, not the \
                     pre-flight one:\n{text}"
                );
            }
        }
    }
}

/// A daemon receiver fails the same way. Upstream's daemon skips the
/// change_dir#4 of do_server_recv() (main.c:1212) and reaches
/// get_local_name() with the module-relative destination, so full_fname()
/// names it inside the module.
///
/// upstream: main.c:817-821, util1.c:1528
#[test]
fn daemon_dest_mkdir_failure_reports_file_io() {
    if is_root() {
        eprintln!("skip: running as root, the unwritable-destination path cannot fire");
        return;
    }

    let scratch = TempDir::new().expect("tempdir");
    let src = scratch.path().join("src");
    let module = scratch.path().join("mod");
    let sealed = module.join("ro");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&sealed).expect("mkdir module subdir");
    fs::write(src.join("f.txt"), b"payload").expect("write source file");
    let config = scratch.path().join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "use chroot = no\n[m]\npath = {}\nread only = no\n",
            module.display()
        ),
    )
    .expect("write daemon config");
    let daemon =
        test_support::DaemonProcess::spawn(&config, ["--address=127.0.0.1"]).expect("spawn daemon");

    for client in clients() {
        let guard = ModeGuard(sealed.clone());
        let mut perms = fs::metadata(&sealed)
            .expect("stat module subdir")
            .permissions();
        perms.set_mode(0o555);
        fs::set_permissions(&sealed, perms).expect("chmod module subdir");

        let mut command = Command::new(&client);
        command.arg("-r");
        command.arg(format!("{}/", src.display()));
        command.arg(format!("rsync://127.0.0.1:{}/m/ro/new/", daemon.port()));
        let output = run_with_timeout(command);
        drop(guard);

        assert!(
            !sealed.join("new").exists(),
            "nothing may be created under the sealed parent"
        );
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        assert_eq!(
            output.status.code(),
            Some(11),
            "{client:?}: a daemon push must exit with the daemon's RERR_FILEIO; \
             output was:\n{text}"
        );
        assert_mkdir_report(&client, "\"ro/new\" (in m)", &text);
    }
}
