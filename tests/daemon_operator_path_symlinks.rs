//! Operator-named daemon and batch paths must not be followed through a symlink
//! planted by another user.
//!
//! Upstream opens these through `open_no_attacker_symlinks()` (syscall.c:675),
//! which follows a symlink only when it is owned by uid 0 or the caller's euid:
//!
//! - `params.c:586` - `--config`
//! - `log.c:169` - daemon `log file`, which then falls back to syslog
//!   (log.c:175-182)
//! - `batch.c:267` - `--read-batch`
//! - `clientserver.c:188` - `motd file`, which the oc-only `--motd-file` /
//!   `--motd` flags name as well
//!
//! The PID file takes a different route. `clientserver.c:1636-1652`
//! unlinks a leaf that is not a regular file, then opens the leaf `O_NOFOLLOW`
//! and checks that the opened file is the one `lstat` names. A planted symlink
//! is replaced by a regular file instead of being written through.
//!
//! The upstream 3.5.1 control on these fixtures refuses `--config` ("Failed to
//! parse config file"), leaves the `log file` victim untouched and keeps
//! serving with syslog, replaces the PID-file symlink with a regular file
//! holding the PID, and refuses `--read-batch` with exit 11.
//!
//! WHY THESE NEED ROOT. The plant has to be owned by a uid that is neither 0
//! nor the caller, and only root can `lchown` a symlink to another uid. For a
//! non-root caller every symlink it can create is its own, which the trust
//! rule follows by design. Unprivileged runs print a reason and return.
#![cfg(unix)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{lchown, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use test_support::ReapOnDrop;

const VICTIM: &str = "VICTIM-ORIGINAL\n";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

fn id_of(args: &[&str]) -> Option<u32> {
    let out = Command::new("id").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The uid that owns every plant, or `None` (with a printed reason) when this
/// run cannot plant a symlink owned by someone else.
fn attacker_uid() -> Option<u32> {
    if id_of(&["-u"]) != Some(0) {
        println!("SKIP: needs root to plant a symlink owned by another uid");
        return None;
    }
    let uid = id_of(&["-u", "nobody"]);
    if uid.is_none() {
        println!("SKIP: host has no `nobody` account to own the plant");
    }
    uid
}

/// Plants `link -> target` and hands it to `owner`.
fn plant(link: &Path, target: &Path, owner: u32) {
    symlink(target, link).expect("plant symlink");
    lchown(link, Some(owner), Some(owner)).expect("chown plant");
}

/// Writes a minimal read-only module config listening on `port`.
fn write_config(dir: &Path, port: u16, globals: &str) -> PathBuf {
    let module = dir.join("mod");
    fs::create_dir_all(&module).expect("module dir");
    let conf = dir.join("rsyncd.conf");
    fs::write(
        &conf,
        format!(
            "port = {port}\nuse chroot = no\n{globals}\n[m]\n\tpath = {}\n\tread only = yes\n",
            module.display()
        ),
    )
    .expect("config");
    conf
}

/// A daemon that either reached `listen()` or exited first.
enum Daemon {
    Listening(ReapOnDrop),
    Exited(ExitStatus),
}

/// Starts a daemon and waits until its port answers or the process exits.
///
/// `stdin` is `/dev/null` so the daemon listens instead of taking its inetd
/// path on an inherited terminal or pipe.
fn start_daemon(port: u16, args: &[String]) -> Daemon {
    let mut child = ReapOnDrop::new(
        Command::new(oc_binary())
            .args(["--daemon", "--no-detach"])
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait().expect("poll daemon") {
            return Daemon::Exited(status);
        }
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return Daemon::Listening(child);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("daemon neither listened nor exited within 10s");
}

/// Lists the daemon's modules and returns everything the client printed.
fn list_modules(port: u16) -> String {
    let out = Command::new(oc_binary())
        .arg(format!("rsync://127.0.0.1:{port}/"))
        .output()
        .expect("run client");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

/// `--config` through a foreign-owned symlink is refused before `listen()`;
/// the same config named directly starts normally.
#[test]
fn config_through_untrusted_symlink_is_refused() {
    let Some(owner) = attacker_uid() else { return };
    let Some(port) = free_port() else { return };
    let root = tempfile::tempdir().expect("temp dir");
    let conf = write_config(root.path(), port, "");
    let link = root.path().join("plant.conf");
    plant(&link, &conf, owner);

    match start_daemon(port, &[format!("--config={}", link.display())]) {
        Daemon::Exited(status) => assert!(!status.success(), "refusal must be an error exit"),
        Daemon::Listening(_) => panic!(
            "the daemon read its config through a symlink owned by uid {owner} \
             (params.c:586 opens it with open_no_attacker_symlinks())"
        ),
    }

    // Non-vacuity: the refusal is about the plant, not the config.
    assert!(
        matches!(
            start_daemon(port, &[format!("--config={}", conf.display())]),
            Daemon::Listening(_)
        ),
        "the same config named directly must start the daemon"
    );
}

/// A `log file` behind a foreign-owned symlink is refused, never written
/// through, and the daemon falls back to syslog and keeps serving.
///
/// upstream: log.c:169 refuses the symlink through
/// `open_no_attacker_symlinks()`; log.c:175-182 then switches to syslog, logs
/// the failure and `Ignoring "log file" setting.`, and the daemon carries on.
/// The upstream 3.5.1 control on this fixture serves the transfer, leaves the
/// victim untouched, and writes both lines to syslog.
#[test]
fn log_file_through_untrusted_symlink_falls_back_to_syslog() {
    let Some(owner) = attacker_uid() else { return };
    let Some(port) = free_port() else { return };
    let root = tempfile::tempdir().expect("temp dir");
    let victim = root.path().join("victim");
    fs::write(&victim, VICTIM).expect("victim");
    let link = root.path().join("plant.log");
    plant(&link, &victim, owner);
    let tag = format!("oclogfallback{port}");
    let conf = write_config(
        root.path(),
        port,
        &format!("log file = {}\nsyslog tag = {tag}", link.display()),
    );
    fs::write(root.path().join("mod").join("served"), b"payload\n").expect("module file");
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();

    let Daemon::Listening(daemon) = start_daemon(port, &[format!("--config={}", conf.display())])
    else {
        panic!("a refused log file must not stop the daemon (log.c:175-182)");
    };
    let dest = root.path().join("pulled");
    let status = Command::new(oc_binary())
        .arg("-a")
        .arg(format!("rsync://127.0.0.1:{port}/m/"))
        .arg(&dest)
        .status()
        .expect("run client");
    drop(daemon);

    assert!(status.success(), "the daemon must still serve: {status}");
    assert!(
        dest.join("served").exists(),
        "the pull must deliver the module file"
    );
    assert_eq!(
        fs::read_to_string(&victim).expect("victim"),
        VICTIM,
        "log.c:169 must not append through a symlink owned by uid {owner}"
    );
    assert!(
        fs::symlink_metadata(&link)
            .expect("plant")
            .file_type()
            .is_symlink(),
        "the planted symlink is left alone"
    );
    assert_refusal_in_syslog(&tag, since, &link);
}

/// Checks that the refusal reached syslog, where the daemon now logs.
///
/// journald is the only reader of syslog a test can query; without it the
/// check is reported and skipped, and the unit test
/// `log_file_open_failure_falls_back_to_syslog` still pins the exact lines.
fn assert_refusal_in_syslog(tag: &str, since: u64, link: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let Ok(out) = Command::new("journalctl")
            .args(["-o", "cat", "-t", tag])
            .arg(format!("--since=@{since}"))
            .output()
        else {
            println!("SKIP syslog check: journalctl is not available");
            return;
        };
        if !out.status.success() {
            println!("SKIP syslog check: journal is not readable");
            return;
        }
        let journal = String::from_utf8_lossy(&out.stdout);
        let refused = format!("failed to open log-file {}", link.display());
        if let (Some(failure), Some(ignoring)) = (
            journal.find(&refused),
            journal.find("Ignoring \"log file\" setting."),
        ) {
            assert!(
                failure < ignoring,
                "upstream logs the failure first: {journal:?}"
            );
            return;
        }
        assert!(
            Instant::now() < deadline,
            "syslog never received the refusal under tag {tag}: {journal:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Non-vacuity for the fallback: a plain `log file` path does receive the
/// daemon's log lines, so the symlink case falls back because of the plant.
#[test]
fn plain_log_file_is_written() {
    let Some(_owner) = attacker_uid() else { return };
    let Some(port) = free_port() else { return };
    let root = tempfile::tempdir().expect("temp dir");
    let log = root.path().join("daemon.log");
    let conf = write_config(root.path(), port, &format!("log file = {}", log.display()));
    let Daemon::Listening(daemon) = start_daemon(port, &[format!("--config={}", conf.display())])
    else {
        panic!("the daemon must start with a plain log file");
    };
    list_modules(port);
    drop(daemon);
    assert!(
        !fs::read_to_string(&log).unwrap_or_default().is_empty(),
        "a plain log file must be written"
    );
}

/// A foreign-owned symlink at the PID-file path is replaced by a regular file;
/// its target is never written.
#[test]
fn pid_file_symlink_is_replaced_not_written_through() {
    let Some(owner) = attacker_uid() else { return };
    let Some(port) = free_port() else { return };
    let root = tempfile::tempdir().expect("temp dir");
    let victim = root.path().join("victim");
    fs::write(&victim, VICTIM).expect("victim");
    let pid_path = root.path().join("rsyncd.pid");
    plant(&pid_path, &victim, owner);
    let conf = write_config(
        root.path(),
        port,
        &format!("pid file = {}", pid_path.display()),
    );

    let Daemon::Listening(daemon) = start_daemon(port, &[format!("--config={}", conf.display())])
    else {
        panic!("upstream starts and replaces the planted leaf (clientserver.c:1637)");
    };
    let leaf = fs::symlink_metadata(&pid_path).expect("pid file");
    let recorded = fs::read_to_string(&pid_path).unwrap_or_default();
    let expected_pid = daemon.id().to_string();
    drop(daemon);

    assert_eq!(
        fs::read_to_string(&victim).expect("victim"),
        VICTIM,
        "the PID must never be written through the planted symlink"
    );
    assert!(
        leaf.file_type().is_file(),
        "the leaf must now be a regular file"
    );
    assert_eq!(
        recorded.trim(),
        expected_pid,
        "the new file holds the daemon PID"
    );
}

/// The oc-only `--motd-file` flag reads its path with the same ownership walk
/// as the `motd file` directive (clientserver.c:188).
#[test]
fn motd_file_flag_through_untrusted_symlink_is_never_greeted() {
    let Some(owner) = attacker_uid() else { return };
    let Some(port) = free_port() else { return };
    let root = tempfile::tempdir().expect("temp dir");
    let secret = root.path().join("secret");
    fs::write(&secret, "SECRET-MOTD\n").expect("secret");
    let link = root.path().join("plant.motd");
    plant(&link, &secret, owner);
    let conf = write_config(root.path(), port, "");

    let args = [
        format!("--config={}", conf.display()),
        format!("--motd-file={}", link.display()),
    ];
    if let Daemon::Listening(_daemon) = start_daemon(port, &args) {
        let greeting = list_modules(port);
        assert!(
            !greeting.contains("SECRET-MOTD"),
            "the motd was read through a symlink owned by uid {owner}: {greeting:?}"
        );
    }

    // Non-vacuity: the flag does greet with a plainly named file.
    let Some(port) = free_port() else { return };
    let conf = write_config(root.path(), port, "");
    let args = [
        format!("--config={}", conf.display()),
        format!("--motd-file={}", secret.display()),
    ];
    let Daemon::Listening(_daemon) = start_daemon(port, &args) else {
        panic!("the daemon must start with a plain motd file");
    };
    assert!(
        list_modules(port).contains("SECRET-MOTD"),
        "a plainly named motd file must be greeted with"
    );
}

/// `--read-batch` through a foreign-owned symlink is refused with upstream's
/// exit 11 (batch.c:267-278); the same batch named directly replays.
#[test]
fn read_batch_through_untrusted_symlink_is_refused() {
    let Some(owner) = attacker_uid() else { return };
    let root = tempfile::tempdir().expect("temp dir");
    let src = root.path().join("src");
    fs::create_dir_all(&src).expect("source dir");
    fs::write(src.join("f"), b"payload\n").expect("source file");
    let batch = root.path().join("batch");
    let status = Command::new(oc_binary())
        .arg("-a")
        .arg(format!("--write-batch={}", batch.display()))
        .arg(format!("{}/", src.display()))
        .arg(root.path().join("written"))
        .status()
        .expect("write batch");
    assert!(status.success(), "writing the batch must succeed");
    let link = root.path().join("plant.batch");
    plant(&link, &batch, owner);

    let refused_dest = root.path().join("refused");
    let status = Command::new(oc_binary())
        .arg("-a")
        .arg(format!("--read-batch={}", link.display()))
        .arg(&refused_dest)
        .status()
        .expect("read batch");
    assert_eq!(status.code(), Some(11), "upstream exits RERR_FILEIO (11)");
    assert!(!refused_dest.join("f").exists(), "nothing may be replayed");

    // Non-vacuity: the same batch named directly replays.
    let replayed_dest = root.path().join("replayed");
    let status = Command::new(oc_binary())
        .arg("-a")
        .arg(format!("--read-batch={}", batch.display()))
        .arg(&replayed_dest)
        .status()
        .expect("read batch");
    assert!(status.success(), "the batch named directly must replay");
    assert!(replayed_dest.join("f").exists());
}
