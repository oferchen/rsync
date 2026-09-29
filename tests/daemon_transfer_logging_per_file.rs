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

/// `%u` renders the authenticated user name on every per-file row.
///
/// upstream: log.c `case 'u': n = auth_user;` - `auth_user` is set by
/// `auth_server()` in clientserver.c and stays live for the whole session.
#[test]
fn push_logs_the_authenticated_user_for_percent_u() {
    use std::os::unix::fs::PermissionsExt;

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
    fs::write(src.join("file1.dat"), vec![b'x'; 100]).expect("src file");

    let secrets = root.join("secrets");
    fs::write(&secrets, "alice:sekrit\n").expect("secrets");
    fs::set_permissions(&secrets, fs::Permissions::from_mode(0o600)).expect("chmod secrets");
    let log = root.join("daemon.log");
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
         \tauth users = alice\n\
         \tsecrets file = {secrets}\n\
         \tlog format = %o [%u] %f\n",
        log = log.display(),
        module = module_dir.display(),
        secrets = secrets.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
    let oc = oc_binary();
    let daemon = spawn_daemon(&oc, &root.join("rsyncd.conf"), port);

    let status = Command::new(&oc)
        .env("RSYNC_PASSWORD", "sekrit")
        .args([
            "-rt",
            &format!("{}/", src.display()),
            &format!("rsync://alice@127.0.0.1:{port}/m/"),
        ])
        .status()
        .expect("run push client");
    wait_for_lines(&log, "recv ", 1);
    drop(daemon);
    assert!(status.success(), "authenticated push failed");

    let text = fs::read_to_string(&log).expect("read log");
    // Upstream oracle (rsync 3.5.1, `log format = %o [%u] %f`):
    //   recv [alice] file1.dat
    assert!(
        transfer_lines(&text, "recv ")
            .iter()
            .any(|l| l == "recv [alice] file1.dat"),
        "expected `recv [alice] file1.dat`, got:\n{text}"
    );
}

/// Renders the nine permission characters `ls -l` shows after the type char.
fn perm_string(mode: u32) -> String {
    let rwx = b"rwxrwxrwx";
    (0..9)
        .map(|i| {
            if mode & (0o400 >> i) != 0 {
                rwx[i] as char
            } else {
                '-'
            }
        })
        .collect()
}

/// `%n %L %U %G %M %B` render the entry's name, link, owner, group, mtime and
/// permissions instead of being printed literally.
///
/// upstream: log.c `log_formatted()` - `%n` appends `/` to a directory,
/// `%L` is ` -> target` for a symlink (the target as stored after the
/// daemon's symlink munging), `%U`/`%G` are the preserved ids, `%M` is
/// `timestring(modtime)` with its space turned into `-`, and `%B` is the
/// permission string without its type character.
#[test]
fn push_logs_name_link_owner_group_mtime_and_perms() {
    use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};

    let Some(port) = free_port() else {
        println!("SKIP: no loopback port available");
        return;
    };
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let module_dir = root.join("mod");
    let src = root.join("src");
    fs::create_dir_all(&module_dir).expect("module dir");
    fs::create_dir_all(src.join("d")).expect("src dir");
    fs::write(src.join("d/f"), b"0123456789").expect("src file");
    symlink("d/f", src.join("lnk")).expect("symlink");
    fs::set_permissions(src.join("d/f"), fs::Permissions::from_mode(0o640)).expect("chmod f");
    fs::set_permissions(src.join("d"), fs::Permissions::from_mode(0o750)).expect("chmod d");
    fs::set_permissions(&src, fs::Permissions::from_mode(0o755)).expect("chmod src");
    // 2001-09-09 01:46:40 UTC; the daemon runs with TZ=UTC so `%M` is fixed.
    let stamp = FileTime::from_unix_time(1_000_000_000, 0);
    filetime::set_symlink_file_times(src.join("lnk"), stamp, stamp).expect("stamp lnk");
    for path in [src.join("d/f"), src.join("d"), src.clone()] {
        set_file_mtime(&path, stamp).expect("stamp");
    }
    set_file_mtime(&module_dir, FileTime::from_unix_time(900_000_000, 0)).expect("backdate mod");

    let meta = fs::metadata(src.join("d/f")).expect("stat f");
    let (uid, gid) = (meta.uid(), meta.gid());
    let lnk_perms = perm_string(fs::symlink_metadata(src.join("lnk")).expect("lstat").mode());

    let log = root.join("daemon.log");
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
         \tlog format = %o|%n|%L|%U|%G|%M|%B|%i\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
    let oc = oc_binary();
    let daemon = ReapOnDrop::new(
        Command::new(&oc)
            .env("TZ", "UTC")
            .arg("--daemon")
            .arg("--no-detach")
            .arg(format!("--config={}", root.join("rsyncd.conf").display()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn daemon"),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && std::net::TcpStream::connect(("127.0.0.1", port)).is_err() {
        std::thread::sleep(Duration::from_millis(20));
    }

    let status = Command::new(&oc)
        .args([
            "-rlptgo",
            &format!("{}/", src.display()),
            &format!("rsync://127.0.0.1:{port}/m/"),
        ])
        .status()
        .expect("run push client");
    wait_for_lines(&log, "recv|", 4);
    drop(daemon);
    assert!(status.success(), "push failed");

    let text = fs::read_to_string(&log).expect("read log");
    let lines = transfer_lines(&text, "recv|");
    // Upstream oracle (rsync 3.5.1 daemon, `use chroot = no`, which turns on
    // `munge symlinks`, so the stored target carries `/rsyncd-munged/`).
    let m = "2001/09/09-01:46:40";
    for want in [
        format!("recv|./||{uid}|{gid}|{m}|rwxr-xr-x|.d..t......"),
        format!("recv|d/||{uid}|{gid}|{m}|rwxr-x---|cd+++++++++"),
        format!("recv|d/f||{uid}|{gid}|{m}|rw-r-----|>f+++++++++"),
        format!("recv|lnk| -> /rsyncd-munged/d/f|{uid}|{gid}|{m}|{lnk_perms}|cL+++++++++"),
    ] {
        assert!(
            lines.iter().any(|l| l == &want),
            "expected `{want}`, got:\n{text}"
        );
    }
}

/// Without `-o`/`-g` upstream has no `uid_ndx`/`gid_ndx`, so `%U` renders 0
/// and `%G` renders `DEFAULT` (log.c `case 'U'` / `case 'G'`). A pull
/// exercises this: the daemon sender scans real ids from disk and must still
/// drop them, whereas a push never puts them on the wire.
#[test]
fn pull_without_owner_or_group_logs_zero_uid_and_default_gid() {
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
    fs::write(module_dir.join("file1.dat"), vec![b'y'; 100]).expect("module file");

    let log = root.join("daemon.log");
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
         \tlog format = %o|%f|%U|%G\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
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
    wait_for_lines(&log, "send|", 1);
    drop(daemon);
    assert!(status.success(), "pull failed");

    let text = fs::read_to_string(&log).expect("read log");
    assert!(
        transfer_lines(&text, "send|")
            .iter()
            .any(|l| l == "send|file1.dat|0|DEFAULT"),
        "expected `send|file1.dat|0|DEFAULT`, got:\n{text}"
    );
}

/// A non-root daemon receiver that cannot set an entry's group logs
/// `DEFAULT` for `%G`, because uidlist.c:284 marks the entry
/// `FLAG_SKIP_GROUP` and log.c `case 'G'` tests that flag.
///
/// Skip condition (the test passes with a printed reason): running as root,
/// or no readable system file belongs to a group this process is not in.
#[test]
fn push_of_foreign_group_file_logs_default_gid() {
    use std::os::unix::fs::MetadataExt;

    let Some(port) = free_port() else {
        println!("SKIP: no loopback port available");
        return;
    };
    let groups = Command::new("id")
        .arg("-G")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .unwrap_or_default();
    let my_groups: Vec<u32> = groups
        .split_whitespace()
        .filter_map(|g| g.parse().ok())
        .collect();
    let uid = Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|s| s.trim().parse::<u32>().ok());
    if my_groups.is_empty() || uid == Some(0) {
        println!("SKIP: running as root or group list unavailable");
        return;
    }
    let foreign = ["/etc/hosts", "/etc/passwd", "/bin/sh", "/usr/bin/env"]
        .iter()
        .filter_map(|p| fs::canonicalize(p).ok())
        .find(|p| {
            fs::metadata(p).is_ok_and(|m| m.is_file() && !my_groups.contains(&m.gid()))
                && fs::File::open(p).is_ok()
        });
    let Some(foreign) = foreign else {
        println!("SKIP: no readable file owned by a group outside {my_groups:?}");
        return;
    };
    let base = foreign
        .file_name()
        .expect("file name")
        .to_string_lossy()
        .into_owned();

    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let module_dir = root.join("mod");
    fs::create_dir_all(&module_dir).expect("module dir");
    let log = root.join("daemon.log");
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
         \tlog format = %o|%f|%G\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
    let oc = oc_binary();
    let daemon = spawn_daemon(&oc, &root.join("rsyncd.conf"), port);

    let status = Command::new(&oc)
        .args([
            "-tg",
            &foreign.display().to_string(),
            &format!("rsync://127.0.0.1:{port}/m/"),
        ])
        .status()
        .expect("run push client");
    wait_for_lines(&log, "recv|", 1);
    drop(daemon);
    assert!(status.success(), "push of {} failed", foreign.display());

    let text = fs::read_to_string(&log).expect("read log");
    let want = format!("recv|{base}|DEFAULT");
    assert!(
        transfer_lines(&text, "recv|").iter().any(|l| l == &want),
        "expected `{want}`, got:\n{text}"
    );
}

/// Runs one transfer against a fresh daemon whose module logs
/// `%o|%f|%C|%i`, and returns the `op` lines it wrote.
///
/// `prepare` seeds the module before the daemon starts; `push` selects the
/// direction. The source tree is `d/f` holding `0123456789`.
fn checksum_log_lines(
    client_args: &[&str],
    push: bool,
    prepare: impl FnOnce(&Path, &Path),
) -> Option<Vec<String>> {
    let port = free_port()?;
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let module_dir = root.join("mod");
    let src = root.join("src");
    let dest = root.join("dest");
    for dir in [&module_dir, &src.join("d"), &dest] {
        fs::create_dir_all(dir).expect("mkdir");
    }
    fs::write(src.join("d/f"), b"0123456789").expect("src file");
    prepare(&src, &module_dir);

    let log = root.join("daemon.log");
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
         \tlog format = %o|%f|%C|%i\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
    let oc = oc_binary();
    let daemon = spawn_daemon(&oc, &root.join("rsyncd.conf"), port);

    let url = format!("rsync://127.0.0.1:{port}/m/");
    let local = format!("{}/", if push { &src } else { &dest }.display());
    let (from, to, op) = if push {
        (local.as_str(), url.as_str(), "recv|")
    } else {
        (url.as_str(), local.as_str(), "send|")
    };
    let status = Command::new(&oc)
        .args(client_args)
        .args([from, to])
        .status()
        .expect("run client");
    wait_for_lines(&log, &format!("{op}d/f|"), 1);
    drop(daemon);
    assert!(status.success(), "transfer {client_args:?} failed");
    let text = fs::read_to_string(&log).expect("read log");
    Some(transfer_lines(&text, op))
}

fn assert_has_line(lines: &[String], want: &str) {
    assert!(
        lines.iter().any(|l| l == want),
        "expected `{want}`, got:\n{}",
        lines.join("\n")
    );
}

/// Upstream oracle (rsync 3.5.1 daemon, lxhost): the whole-file sum of
/// `0123456789` under the negotiated xxh128, printed byte-reversed by
/// `sum_as_hex()` because `canonical_checksum(CSUM_XXH3_128)` is 1.
const XXH128_HEX: &str = "e353667619ec664b49655fc9692165fb";
/// The same content under `--checksum-choice=md5`, printed in digest order.
const MD5_HEX: &str = "781e5e245d69b566979b86e28d23f2c7";

/// `%C` shows the transfer sum for a transferred regular file and
/// `csum_len * 2` spaces for a directory (log.c `case 'C'`).
#[test]
fn push_logs_the_transfer_checksum_for_percent_c() {
    let Some(lines) = checksum_log_lines(&["-r"], true, |_, _| {}) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, &format!("recv|d/f|{XXH128_HEX}|>f+++++++++"));
    assert_has_line(&lines, &format!("recv|d|{}|cd+++++++++", " ".repeat(32)));
}

/// The digest follows the negotiated checksum: MD5 prints in digest order.
#[test]
fn push_with_md5_logs_the_md5_sum_for_percent_c() {
    let Some(lines) = checksum_log_lines(&["-r", "--checksum-choice=md5"], true, |_, _| {}) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, &format!("recv|d/f|{MD5_HEX}|>f+++++++++"));
}

/// Under `--checksum` a regular file shows its file-list sum even when no data
/// moves: a permissions-only change still logs the digest.
#[test]
fn push_with_checksum_logs_the_file_list_sum_for_an_untransferred_file() {
    use std::os::unix::fs::PermissionsExt;

    let Some(lines) = checksum_log_lines(&["-rpc"], true, |src, module| {
        fs::create_dir_all(module.join("d")).expect("module d");
        fs::copy(src.join("d/f"), module.join("d/f")).expect("seed f");
        fs::set_permissions(src.join("d/f"), fs::Permissions::from_mode(0o644)).expect("chmod");
        fs::set_permissions(module.join("d/f"), fs::Permissions::from_mode(0o600)).expect("chmod");
    }) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, &format!("recv|d/f|{XXH128_HEX}|.f...p....."));
}

/// A daemon sender logs the sum it computed while sending.
#[test]
fn pull_logs_the_sent_checksum_for_percent_c() {
    let Some(lines) = checksum_log_lines(&["-r"], false, |src, module| {
        fs::create_dir_all(module.join("d")).expect("module d");
        fs::copy(src.join("d/f"), module.join("d/f")).expect("seed f");
    }) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, &format!("send|d/f|{XXH128_HEX}|<f+++++++++"));
}
