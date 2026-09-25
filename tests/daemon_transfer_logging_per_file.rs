//! A daemon module with `transfer logging = yes` must write ONE log-file line
//! per processed file, not a single empty summary line.
//!
//! Upstream reaches this through the per-file `log_item(FLOG)` calls that fire
//! inside both transfer roles:
//!
//! ```c
//! /* receiver.c:823 */  int itemizing = am_server ? logfile_format_has_i : ...;
//! /* receiver.c:919 */  maybe_log_item(file, iflags, itemizing, xname);   /* non-transfer */
//! /* receiver.c:1290 */ log_item(log_code, file, iflags, NULL);           /* per transfer */
//! /* sender.c:500/585 */ ... mirror on the daemon-sender (pull) side.
//! ```
//!
//! With `log format = %o %f %l %i` a push of three new files therefore logs the
//! transfer root `.` (a `.d..t......` metadata row when its mtime differs) plus
//! one `>f+++++++++` row per file, and a pull logs one `<f+++++++++` row per
//! file. oc previously emitted a SINGLE `recv  0 ` summary line with an empty
//! `%f`/`%l`/`%i`; this test pins the per-entry behaviour and the direction
//! glyphs.
//!
//! Skip condition (the test passes with a printed reason): loopback TCP is
//! unavailable. The expected lines are pinned from the measured upstream 3.5.x
//! output; they are labelled as the upstream oracle.

#![cfg(unix)]

use std::fs;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use filetime::{FileTime, set_file_mtime};
use test_support::ReapOnDrop;

const LOG_FORMAT: &str = "%o %f %l %i";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// Writes an rsyncd.conf for a single read-write module named `m` that logs
/// every transfer with `LOG_FORMAT`, and returns the temp root and port.
fn make_daemon(root: &Path, port: u16, module_dir: &Path, log: &Path) {
    let conf = format!(
        "port = {port}\n\
         use chroot = no\n\
         log file = {log}\n\
         reverse lookup = no\n\
         \n\
         [m]\n\
         \tpath = {module}\n\
         \tread only = no\n\
         \ttransfer logging = yes\n\
         \tlog format = {LOG_FORMAT}\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
}

fn spawn_daemon(binary: &Path, conf: &Path, port: u16) -> ReapOnDrop {
    let child = ReapOnDrop::new(
        Command::new(binary)
            .arg("--daemon")
            .arg("--no-detach")
            .arg(format!("--config={}", conf.display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return child;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
}

/// Blocks until the daemon log holds at least `want` transfer-op lines, so a
/// caller never races the post-disconnect FLOG writes.
fn wait_for_lines(log: &Path, op: &str, want: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Ok(text) = fs::read_to_string(log) {
            let count = text.lines().filter(|l| l.contains(op)).count();
            if count >= want {
                return;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Returns the per-file transfer-log lines (those carrying the `recv`/`send`
/// operation the module format renders), trimmed to the formatted body.
fn transfer_lines(log: &str, op: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| line.find(op).map(|start| line[start..].to_owned()))
        .collect()
}

/// A push of three new files logs the transfer root and one `>f+++++++++` row
/// per file, in flist order.
#[test]
fn push_logs_one_recv_line_per_entry() {
    let Some(port) = free_port() else {
        println!("SKIP: no loopback port available");
        return;
    };
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let module_dir = root.join("mod");
    let src = root.join("src");
    fs::create_dir_all(&module_dir).expect("module dir");
    fs::create_dir_all(&src).expect("src dir");
    for (name, size) in [
        ("file1.dat", 2000usize),
        ("file2.dat", 3000),
        ("file3.dat", 4000),
    ] {
        fs::write(src.join(name), vec![b'x'; size]).expect("src file");
    }
    // Backdate the destination module directory so the pushed root `.` reports a
    // metadata-only `.d..t......` time row (upstream generator.c:526-530), making
    // the dir line deterministic rather than dependent on same-second timing.
    let old = FileTime::from_unix_time(1_000_000_000, 0);
    set_file_mtime(&module_dir, old).expect("backdate module dir");

    let log = root.join("daemon.log");
    make_daemon(root, port, &module_dir, &log);
    let oc = oc_binary();
    let daemon = spawn_daemon(&oc, &root.join("rsyncd.conf"), port);

    let status = Command::new(&oc)
        .args([
            "-rt",
            &format!("{}/", src.display()),
            &format!("rsync://127.0.0.1:{port}/m/"),
        ])
        .status()
        .expect("run push client");
    wait_for_lines(&log, "recv ", 4);
    drop(daemon);
    assert!(status.success(), "push failed");

    let text = fs::read_to_string(&log).expect("read log");
    let lines = transfer_lines(&text, "recv ");

    // Upstream oracle (rsync 3.5.x, `log format = %o %f %l %i`, push 3 files):
    //   recv . 160 .d..t......
    //   recv file1.dat 2000 >f+++++++++
    //   recv file2.dat 3000 >f+++++++++
    //   recv file3.dat 4000 >f+++++++++
    // The dir `%l` (its st_size) is filesystem-dependent, so match the glyph and
    // name rather than a pinned dir size.
    assert!(
        lines
            .iter()
            .any(|l| l.starts_with("recv . ") && l.ends_with(".d..t......")),
        "expected the transfer-root dir row `recv . <size> .d..t......`, got:\n{text}"
    );
    for (name, size) in [
        ("file1.dat", 2000),
        ("file2.dat", 3000),
        ("file3.dat", 4000),
    ] {
        let want = format!("recv {name} {size} >f+++++++++");
        assert!(
            lines.iter().any(|l| l == &want),
            "expected per-file line `{want}`, got:\n{text}"
        );
    }
    // Exactly one line per entry - never the old single empty summary, and never
    // a duplicate per file.
    assert_eq!(
        lines.len(),
        4,
        "expected 4 per-entry recv lines (dir + 3 files), got {}:\n{text}",
        lines.len()
    );
    // The retired summary line had an empty %f/%l/%i - it must be gone.
    assert!(
        !lines.iter().any(|l| l == "recv  0 "),
        "the empty summary line must not appear:\n{text}"
    );
}

/// A pull of three new files logs one `<f+++++++++` row per file on the daemon
/// sender; the direction glyph is `<` (op `send`), and there is no dir row (the
/// client receiver creates and logs directories on its own side).
#[test]
fn pull_logs_one_send_line_per_file() {
    let Some(port) = free_port() else {
        println!("SKIP: no loopback port available");
        return;
    };
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let module_dir = root.join("mod");
    let dest = root.join("dest");
    fs::create_dir_all(&module_dir).expect("module dir");
    fs::create_dir_all(&dest).expect("dest dir");
    for (name, size) in [
        ("file1.dat", 2000usize),
        ("file2.dat", 3000),
        ("file3.dat", 4000),
    ] {
        fs::write(module_dir.join(name), vec![b'y'; size]).expect("module file");
    }
    // Backdate the module files so quick-check cannot skip them against the
    // freshly-created (empty) destination - they are new at the dest anyway, but
    // this keeps the transfer deterministic across filesystems.
    let old =
        FileTime::from_system_time(SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000));
    for name in ["file1.dat", "file2.dat", "file3.dat"] {
        set_file_mtime(module_dir.join(name), old).expect("backdate");
    }

    let log = root.join("daemon.log");
    make_daemon(root, port, &module_dir, &log);
    let oc = oc_binary();
    let daemon = spawn_daemon(&oc, &root.join("rsyncd.conf"), port);

    let status = Command::new(&oc)
        .args([
            "-rt",
            &format!("rsync://127.0.0.1:{port}/m/"),
            &format!("{}/", dest.display()),
        ])
        .status()
        .expect("run pull client");
    wait_for_lines(&log, "send ", 3);
    drop(daemon);
    assert!(status.success(), "pull failed");

    let text = fs::read_to_string(&log).expect("read log");
    let lines = transfer_lines(&text, "send ");

    // Upstream oracle (rsync 3.5.x, `log format = %o %f %l %i`, pull 3 files):
    //   send file1.dat 2000 <f+++++++++
    //   send file2.dat 3000 <f+++++++++
    //   send file3.dat 4000 <f+++++++++
    for (name, size) in [
        ("file1.dat", 2000),
        ("file2.dat", 3000),
        ("file3.dat", 4000),
    ] {
        let want = format!("send {name} {size} <f+++++++++");
        assert!(
            lines.iter().any(|l| l == &want),
            "expected per-file line `{want}`, got:\n{text}"
        );
    }
    assert_eq!(
        lines.len(),
        3,
        "expected 3 per-file send lines, got {}:\n{text}",
        lines.len()
    );
}
