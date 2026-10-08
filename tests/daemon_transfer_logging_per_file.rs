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
    spawn_daemon_with(binary, conf, port, &[])
}

/// Starts the daemon with extra command-line arguments. It runs with `TZ=UTC`
/// so a `%M` field renders a fixed wall-clock string.
fn spawn_daemon_with(binary: &Path, conf: &Path, port: u16, args: &[&str]) -> ReapOnDrop {
    let child = ReapOnDrop::new(
        Command::new(binary)
            .env("TZ", "UTC")
            .args(args)
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

/// Runs one transfer against a fresh daemon whose module logs `format`
/// (which must start `%o|%f|`), and returns the `op` lines it wrote.
///
/// `prepare` seeds the module before the daemon starts; `push` selects the
/// direction. The source tree is `d/f` holding `0123456789`.
fn module_log_lines(
    format: &str,
    client_args: &[&str],
    push: bool,
    prepare: impl FnOnce(&Path, &Path),
) -> Option<Vec<String>> {
    let op = if push { "recv|" } else { "send|" };
    let text = module_log(
        format,
        &[],
        client_args,
        push,
        prepare,
        &format!("{op}d/f|"),
    )?;
    Some(transfer_lines(&text, op))
}

/// Runs one transfer against a fresh daemon started with `daemon_args` whose
/// module logs `format`, waits for a log line containing `marker`, and returns
/// the whole log.
fn module_log(
    format: &str,
    daemon_args: &[&str],
    client_args: &[&str],
    push: bool,
    prepare: impl FnOnce(&Path, &Path),
    marker: &str,
) -> Option<String> {
    module_log_at(format, daemon_args, client_args, push, prepare, marker, "")
}

/// [`module_log`] against `rsync://host/m/{tail}` instead of the module root.
fn module_log_at(
    format: &str,
    daemon_args: &[&str],
    client_args: &[&str],
    push: bool,
    prepare: impl FnOnce(&Path, &Path),
    marker: &str,
    tail: &str,
) -> Option<String> {
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
         \tlog format = {format}\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
    let oc = oc_binary();
    let daemon = spawn_daemon_with(&oc, &root.join("rsyncd.conf"), port, daemon_args);

    let url = format!("rsync://127.0.0.1:{port}/m/{tail}");
    let local = format!("{}/", if push { &src } else { &dest }.display());
    let (from, to) = if push {
        (local.as_str(), url.as_str())
    } else {
        (url.as_str(), local.as_str())
    };
    let status = Command::new(&oc)
        .args(client_args)
        .args([from, to])
        .status()
        .expect("run client");
    wait_for_lines(&log, marker, 1);
    drop(daemon);
    assert!(status.success(), "transfer {client_args:?} failed");
    Some(fs::read_to_string(&log).expect("read log"))
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
const CHECKSUM_FORMAT: &str = "%o|%f|%C|%i";

/// `%C` shows the transfer sum for a transferred regular file and
/// `csum_len * 2` spaces for a directory (log.c `case 'C'`).
#[test]
fn push_logs_the_transfer_checksum_for_percent_c() {
    let Some(lines) = module_log_lines(CHECKSUM_FORMAT, &["-r"], true, |_, _| {}) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, &format!("recv|d/f|{XXH128_HEX}|>f+++++++++"));
    assert_has_line(&lines, &format!("recv|d|{}|cd+++++++++", " ".repeat(32)));
}

/// The digest follows the negotiated checksum: MD5 prints in digest order.
#[test]
fn push_with_md5_logs_the_md5_sum_for_percent_c() {
    let Some(lines) = module_log_lines(
        CHECKSUM_FORMAT,
        &["-r", "--checksum-choice=md5"],
        true,
        |_, _| {},
    ) else {
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

    let Some(lines) = module_log_lines(CHECKSUM_FORMAT, &["-rpc"], true, |src, module| {
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
    let Some(lines) = module_log_lines(CHECKSUM_FORMAT, &["-r"], false, |src, module| {
        fs::create_dir_all(module.join("d")).expect("module d");
        fs::copy(src.join("d/f"), module.join("d/f")).expect("seed f");
    }) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, &format!("send|d/f|{XXH128_HEX}|<f+++++++++"));
}

const BYTES_FORMAT: &str = "%o|%f|%b|%c|%i";

/// A pushed new file logs the payload the daemon receiver read for it and no
/// checksum bytes; a directory logs neither.
///
/// Upstream oracle (rsync 3.5.1 daemon, lxhost): `%b` = sum head (16) +
/// literal token (4 + 10) + end token (4) + xxh128 file sum (16); `%c` counts
/// the receiver's writes in the window, of which there are none.
#[test]
fn push_logs_received_bytes_for_percent_b_and_c() {
    let Some(lines) = module_log_lines(BYTES_FORMAT, &["-r"], true, |_, _| {}) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, "recv|d/f|50|0|>f+++++++++");
    assert_has_line(&lines, "recv|d|0|0|cd+++++++++");
}

/// A daemon sender logs what it wrote (the echoed ndx and iflags, sum head,
/// tokens and file sum) as `%b`, and the sum head it read as `%c`.
///
/// Upstream oracle (rsync 3.5.1 daemon, lxhost): `%b` = 3 + 50, `%c` = 16.
#[test]
fn pull_logs_sent_and_checksum_bytes_for_percent_b_and_c() {
    let Some(lines) = module_log_lines(BYTES_FORMAT, &["-r"], false, |src, module| {
        fs::create_dir_all(module.join("d")).expect("module d");
        fs::copy(src.join("d/f"), module.join("d/f")).expect("seed f");
    }) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines, "send|d/f|53|16|<f+++++++++");
}

/// Seeds the module with the extraneous entries the delete tests expect the
/// push to remove: a file `extra` and a directory `xdir` holding `y`.
fn seed_extraneous(module: &Path) {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(module.join("xdir")).expect("module xdir");
    fs::write(module.join("extra"), b"").expect("seed extra");
    fs::write(module.join("xdir/y"), b"").expect("seed y");
    fs::set_permissions(module.join("extra"), fs::Permissions::from_mode(0o644)).expect("chmod");
    fs::set_permissions(module.join("xdir/y"), fs::Permissions::from_mode(0o600)).expect("chmod");
    fs::set_permissions(module.join("xdir"), fs::Permissions::from_mode(0o750)).expect("chmod");
}

/// Returns the body of every line that carries `needle`, from `needle` on.
fn lines_from(log: &str, needle: &str) -> Vec<String> {
    log.lines()
        .filter_map(|line| line.find(needle).map(|start| line[start..].to_owned()))
        .collect()
}

const DELETE_FORMAT: &str = "%o|%f|%n|%l|%U|%G|%M|%B|%b|%c|%C|%i";

// Ordering of `del.` rows against the per-file rows: upstream's generator
// deletes while its receiver process logs transfers, so the interleaving in
// the module log follows process scheduling and is not deterministic. oc
// writes every row of an early pass (and every make-room deletion) before the
// per-file rows and every row of a late pass after them, which is one of the
// orders upstream produces; the tests below pin that order.

/// A `--delete` push logs one `del.` row per removed entry, rendered from a
/// zeroed entry that keeps only the victim's mode, ahead of the per-file rows.
///
/// Upstream oracle (rsync 3.5.1 daemon, lxhost, `xferlog_oracle.sh` step 3):
/// log.c:892-929 `log_delete()` renders `del.`, the victim's name (`%n` with a
/// slash for a directory), length/uid 0, gid 0 under `-g`, mtime 0, the real
/// permission bits, zero byte counts, a blank `%C` and `*deleting  `; the
/// generator deletes a directory's descendants first and walks each directory
/// in reverse name order, and it runs before the receiver logs any file.
#[test]
fn push_delete_logs_a_del_row_per_removed_entry() {
    assert_del_rows_lead(&["-a", "--delete"]);
}

/// The leaf-granular executor `--max-delete` selects logs the same rows.
#[test]
fn push_capped_delete_logs_a_del_row_per_removed_entry() {
    assert_del_rows_lead(&["-a", "--delete", "--max-delete=10"]);
}

/// Pushes with `client_args` and asserts the three `del.` rows for the seeded
/// extraneous entries come first, in upstream's order and rendering.
fn assert_del_rows_lead(client_args: &[&str]) {
    let Some(text) = module_log(
        DELETE_FORMAT,
        &[],
        client_args,
        true,
        |_, module| seed_extraneous(module),
        "recv|d/f|",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    let blank = " ".repeat(32);
    let epoch = "1970/01/01-00:00:00";
    let want = [
        format!("del.|xdir/y|xdir/y|0|0|0|{epoch}|rw-------|0|0|{blank}|*deleting  "),
        format!("del.|xdir|xdir/|0|0|0|{epoch}|rwxr-x---|0|0|{blank}|*deleting  "),
        format!("del.|extra|extra|0|0|0|{epoch}|rw-r--r--|0|0|{blank}|*deleting  "),
    ];
    let rows: Vec<String> = text
        .lines()
        .filter_map(|line| {
            ["del.|", "recv|"]
                .iter()
                .find_map(|op| line.find(op))
                .map(|start| line[start..].to_owned())
        })
        .collect();
    assert_eq!(rows[..3], want, "del rows and order, got:\n{text}");
    assert!(
        rows[3..].iter().all(|row| row.starts_with("recv|")),
        "every del row precedes the per-file rows, got:\n{text}"
    );
}

/// A format without `%o` or `%i` logs a deletion as the fixed `deleting %n`.
///
/// Upstream oracle (step 6, module `log format = %f %l`): `deleting gone`.
#[test]
fn push_delete_without_o_or_i_logs_deleting_name() {
    let Some(text) = module_log(
        "%f %l",
        &[],
        &["-r", "--delete"],
        true,
        |_, module| seed_extraneous(module),
        "d/f 10",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_eq!(
        lines_from(&text, "deleting "),
        ["deleting xdir/y", "deleting xdir/", "deleting extra"],
        "got:\n{text}"
    );
}

/// An empty `--log-file-format` silences the per-file rows but not the
/// deletions, which fall back to `deleting %n`.
///
/// Upstream oracle (step 10, `--log-file-format=`): `deleting stale` and no
/// per-file row.
#[test]
fn push_delete_with_empty_log_file_format_logs_only_deleting_lines() {
    let Some(text) = module_log(
        DELETE_FORMAT,
        &["--log-file-format="],
        &["-r", "--delete"],
        true,
        |_, module| seed_extraneous(module),
        "deleting extra",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_eq!(
        lines_from(&text, "deleting "),
        ["deleting xdir/y", "deleting xdir/", "deleting extra"],
        "got:\n{text}"
    );
    assert!(
        lines_from(&text, "recv|").is_empty() && lines_from(&text, "del.|").is_empty(),
        "an empty format writes no per-file row, got:\n{text}"
    );
}

/// A dry run deletes nothing and logs no deletion (log.c:924 `dry_run`).
#[test]
fn push_delete_dry_run_logs_no_deletion() {
    let Some(text) = module_log(
        DELETE_FORMAT,
        &[],
        &["-rn", "--delete"],
        true,
        |_, module| seed_extraneous(module),
        "total size",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert!(lines_from(&text, "del.|").is_empty(), "got:\n{text}");
}

/// `--delete-after` deletes once every file has landed, so its rows follow the
/// per-file rows.
#[test]
fn push_delete_after_logs_del_rows_after_the_transfer_rows() {
    assert_del_rows_trail("--delete-after");
}

/// `--delete-delay` decides during the walk but unlinks, and so logs, only
/// after the transfer (generator.c:2419 `do_delayed_deletions()`).
#[test]
fn push_delete_delay_logs_del_rows_after_the_transfer_rows() {
    assert_del_rows_trail("--delete-delay");
}

fn assert_del_rows_trail(mode: &str) {
    let Some(text) = module_log(
        DELETE_FORMAT,
        &[],
        &["-r", mode],
        true,
        |_, module| seed_extraneous(module),
        "del.|extra|",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    let last_recv = lines.iter().rposition(|l| l.contains("recv|"));
    let first_del = lines.iter().position(|l| l.contains("del.|"));
    assert!(
        matches!((last_recv, first_del), (Some(r), Some(d)) if r < d),
        "got:\n{text}"
    );
}

/// upstream: log.c `case 'f'` - a daemon receiver joins its `curr_dir` below
/// the module root in front of each name, so a push into `m/a/b/` logs
/// `a/b/d/f` while `%n` stays transfer-relative (measured against rsync 3.5.1).
#[test]
fn push_into_a_module_subdirectory_prefixes_percent_f() {
    let Some(text) = module_log_at(
        "%o|%f|%n",
        &[],
        &["-rt"],
        true,
        |_, module| fs::create_dir_all(module.join("a")).expect("mkdir"),
        "recv|a/b/d/f|",
        "a/b/",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    let lines = transfer_lines(&text, "recv|");
    assert_has_line(&lines, "recv|a/b/d/f|d/f");
}

/// A deletion below a module subdirectory carries the same prefix as the
/// transfer rows (log.c:924 `log_formatted()` takes the same `case 'f'`).
#[test]
fn push_delete_into_a_module_subdirectory_prefixes_del_rows() {
    let Some(text) = module_log_at(
        "%o|%f|%n",
        &[],
        &["-rt", "--delete"],
        true,
        |_, module| {
            fs::create_dir_all(module.join("a")).expect("mkdir");
            fs::write(module.join("a/extra"), b"x").expect("extra");
        },
        "recv|a/d/f|",
        "a/",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_has_line(&lines_from(&text, "del.|"), "del.|a/extra|extra");
}

/// upstream: log.c `case 'f'` - a sender joins `F_PATHNAME(file)`, the
/// positional's directory below the module root, so a pull of `m/p/sub/`
/// logs `p/sub/g` (measured against rsync 3.5.1).
#[test]
fn pull_from_a_module_subdirectory_prefixes_percent_f() {
    let Some(text) = module_log_at(
        "%o|%f|%n",
        &[],
        &["-rt"],
        false,
        |_, module| {
            fs::create_dir_all(module.join("p/sub")).expect("mkdir");
            fs::write(module.join("p/sub/g"), b"g").expect("file");
        },
        "send|p/sub/g|",
        "p/sub/",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    let lines = transfer_lines(&text, "send|");
    assert_has_line(&lines, "send|p/sub/g|g");
}

/// upstream: receiver.c:722 writes the lone file of a single-file push to the
/// destination's basename (`local_name`), but log_item() renders
/// `f_name(file)`, so the log keeps the file-list name below the parent
/// directory (measured against rsync 3.5.1: `recv|a/t0|t0`).
#[test]
fn single_file_push_to_a_new_name_logs_the_file_list_name() {
    let Some(port) = free_port() else {
        println!("SKIP: no loopback port available");
        return;
    };
    let tmp = tempfile::tempdir().expect("temp dir");
    let root = tmp.path();
    let module_dir = root.join("mod");
    fs::create_dir_all(module_dir.join("a")).expect("mkdir");
    let src = root.join("t0");
    fs::write(&src, b"t").expect("src file");
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
         \tlog format = %o|%f|%n\n",
        log = log.display(),
        module = module_dir.display(),
    );
    fs::write(root.join("rsyncd.conf"), conf).expect("config");
    let oc = oc_binary();
    let daemon = spawn_daemon_with(&oc, &root.join("rsyncd.conf"), port, &[]);
    let status = Command::new(&oc)
        .arg("-t")
        .arg(&src)
        .arg(format!("rsync://127.0.0.1:{port}/m/a/newname"))
        .status()
        .expect("run client");
    wait_for_lines(&log, "recv|", 1);
    drop(daemon);
    assert!(status.success(), "single-file push failed");
    assert!(module_dir.join("a/newname").is_file(), "file not renamed");
    let text = fs::read_to_string(&log).expect("read log");
    assert_has_line(&transfer_lines(&text, "recv|"), "recv|a/t0|t0");
}

/// upstream: delete.c:126 - a file replacing a non-empty directory clears it
/// through delete_dir_contents(), which drops `DEL_MAKE_ROOM` before it
/// recurses, so every entry inside is logged while the directory itself is
/// not (measured against rsync 3.5.1: `x/sub/b`, `x/sub`, `x/a`, no `x`).
#[test]
fn push_file_over_non_empty_dir_logs_its_contents_as_del_rows() {
    let Some(text) = module_log(
        "%o|%f|%n|%i",
        &[],
        &["-rt", "--delete"],
        true,
        |src, module| {
            fs::write(src.join("x"), b"x").expect("src file");
            fs::create_dir_all(module.join("x/sub")).expect("mkdir");
            fs::write(module.join("x/a"), b"a").expect("a");
            fs::write(module.join("x/sub/b"), b"b").expect("b");
        },
        "recv|x|",
    ) else {
        println!("SKIP: no loopback port available");
        return;
    };
    assert_eq!(
        lines_from(&text, "del.|"),
        [
            "del.|x/sub/b|x/sub/b|*deleting  ",
            "del.|x/sub|x/sub/|*deleting  ",
            "del.|x/a|x/a|*deleting  ",
        ],
        "log:\n{text}"
    );
}
