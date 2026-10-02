//! Operator paths on a `path = /` module served from its working directory.
//!
//! A module with a privilege drop (`uid =`) and no chroot is entered before the
//! drop and then served relative to that working directory. Upstream does the
//! same (`change_dir(module_chdir)`, clientserver.c:1059) but keeps
//! `module_dir` as the real module path, and that is the root every
//! peer-supplied operator path is sanitized against:
//!
//! - `--backup-dir`: `sanitize_path(NULL, backup_dir, NULL, 0, SP_DEFAULT)`
//!   (options.c:2417), then the daemon-filter check (options.c:2433-2440).
//! - `--partial-dir` and `--link-dest`: `sanitize_path(NULL, dir, NULL,
//!   curr_dir_depth, SP_DEFAULT)` (main.c:1254-1257), then the daemon-filter
//!   check (main.c:1261-1284).
//!
//! Rooting those at the served spelling (`.`) instead of `module_dir` let a
//! `..` traversal slip past the module's `exclude` and made an in-module
//! `--link-dest=../01` resolve nowhere. These cells mirror upstream's
//! `operator-path-traversal-backup-dir-daemon`,
//! `operator-path-traversal-partial-dir-daemon` and `link-dest-pathroot`
//! tests, with `uid =` set to the test user so the served-root path runs
//! without root.
#![cfg(unix)]

use std::fs;
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use test_support::ReapOnDrop;

const SECRET: &[u8] = b"PROTECTED-IN-EXCLUDED-SUBTREE\n";

fn oc_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn free_port() -> Option<u16> {
    TcpListener::bind("127.0.0.1:0")
        .ok()
        .and_then(|listener| listener.local_addr().ok())
        .map(|addr| addr.port())
}

/// A test tree plus a daemon serving `/` as module `m`, dropping to the owner
/// of the tree so the module is entered before the drop and served as `.`.
struct Served {
    root: PathBuf,
    port: u16,
    _daemon: ReapOnDrop,
    _temp: tempfile::TempDir,
}

impl Served {
    /// `exclude` is the module's `exclude` value, if any.
    fn start(setup: impl FnOnce(&Path) -> Option<String>) -> Option<Self> {
        let port = free_port()?;
        let temp = tempfile::tempdir().expect("tempdir");
        let root = fs::canonicalize(temp.path()).expect("canonicalize tempdir");
        if root.to_string_lossy().contains(' ') {
            // `exclude` is a space-separated list, as in upstream's own cells.
            return None;
        }
        let exclude = setup(&root);
        let uid = fs::metadata(&root).expect("stat tempdir").uid();
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
                 \tpath = /\n\
                 \tread only = no\n\
                 \tuid = {uid}\n\
                 {exclude}",
                log = root.join("daemon.log").display(),
                exclude = exclude.map_or_else(String::new, |e| format!("\texclude = {e}\n")),
            ),
        )
        .expect("write config");
        let daemon = ReapOnDrop::new(
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
        Some(Self {
            root,
            port,
            _daemon: daemon,
            _temp: temp,
        })
    }

    /// `path` addressed through the `path = /` module.
    fn url(&self, path: &Path) -> String {
        let rel = path.to_str().expect("utf-8 path").trim_start_matches('/');
        format!("rsync://127.0.0.1:{}/m/{rel}", self.port)
    }

    /// `path` spelled as the module-rooted absolute value a peer sends.
    fn module_abs(path: &Path) -> String {
        path.to_str().expect("utf-8 path").to_owned()
    }

    fn push(&self, args: &[String]) -> Output {
        Command::new(oc_binary())
            .arg("--timeout=20")
            .args(args)
            .stdin(Stdio::null())
            .output()
            .expect("run push client")
    }

    fn log(&self) -> String {
        fs::read_to_string(self.root.join("daemon.log")).unwrap_or_default()
    }
}

/// Lays out upstream's traversal fixture: `<root>/secret/f0` is excluded, and
/// `<root>/<leg>/src/` pushes a changed `f0` over `<root>/<leg>/dest/f0`.
fn traversal_fixture(root: &Path, leg: &str) -> String {
    let secret = root.join("secret");
    fs::create_dir(&secret).expect("create secret");
    fs::write(secret.join("f0"), SECRET).expect("write victim");
    let dest = root.join(leg).join("dest");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(dest.join("f0"), b"OLD-DESTINATION-FILE-CONTENT\n").expect("seed dest");
    let src = root.join(leg).join("src");
    fs::create_dir_all(src.join("sub")).expect("create src");
    fs::write(src.join("f0"), b"NEW-PUSHED-CONTENT\n").expect("write src");
    format!("{}/", secret.display())
}

/// upstream: options.c:2417 roots an absolute `--backup-dir` at `module_dir`
/// and options.c:2433-2440 then refuses it when the module excludes it, so a
/// `..` climb back into the excluded subtree must not replace the victim.
#[test]
fn a_backup_dir_traversal_cannot_reach_the_excluded_subtree() {
    let Some(served) = Served::start(|root| Some(traversal_fixture(root, "bk"))) else {
        println!("SKIP: no loopback port, or a scratch path with a space");
        return;
    };
    let leg = served.root.join("bk");
    let traversal = format!("{}/src/sub/../../../secret/", Served::module_abs(&leg));
    let out = served.push(&[
        "-a".into(),
        "--backup".into(),
        format!("--backup-dir={traversal}"),
        format!("{}/", leg.join("src").display()),
        served.url(&leg.join("dest")),
    ]);
    assert_eq!(
        fs::read(served.root.join("secret/f0")).ok().as_deref(),
        Some(SECRET),
        "a --backup-dir '..' traversal reached the excluded subtree\nstderr:\n{}\nlog:\n{}",
        String::from_utf8_lossy(&out.stderr),
        served.log(),
    );
}

/// upstream: main.c:1257 roots an absolute `--partial-dir` at `module_dir` and
/// main.c:1276-1284 refuses it when the module excludes it, so the traversal
/// must neither overwrite nor delete the victim.
#[test]
fn a_partial_dir_traversal_cannot_reach_the_excluded_subtree() {
    let Some(served) = Served::start(|root| Some(traversal_fixture(root, "pd"))) else {
        println!("SKIP: no loopback port, or a scratch path with a space");
        return;
    };
    let leg = served.root.join("pd");
    let traversal = format!("{}/src/sub/../../../secret/", Served::module_abs(&leg));
    let out = served.push(&[
        "-a".into(),
        format!("--partial-dir={traversal}"),
        format!("{}/", leg.join("src").display()),
        served.url(&leg.join("dest")),
    ]);
    assert_eq!(
        fs::read(served.root.join("secret/f0")).ok().as_deref(),
        Some(SECRET),
        "a --partial-dir '..' traversal deleted or overwrote the excluded victim\nstderr:\n{}\nlog:\n{}",
        String::from_utf8_lossy(&out.stderr),
        served.log(),
    );
}

/// upstream: main.c:1254 keeps up to `curr_dir_depth` leading `..` of a
/// relative basis, so `--link-dest=../01` from `00/` reaches the in-module
/// sibling and the file is hard-linked rather than re-sent.
#[test]
fn a_relative_link_dest_climbs_to_the_in_module_sibling() {
    let Some(served) = Served::start(|root| {
        let src = root.join("src");
        let basis = root.join("bak").join("01");
        fs::create_dir_all(&src).expect("create src");
        fs::create_dir_all(&basis).expect("create basis");
        let data: Vec<u8> = (0..40_000u32).map(|i| (i % 251) as u8).collect();
        fs::write(src.join("f.dat"), &data).expect("write src");
        fs::write(basis.join("f.dat"), &data).expect("write basis");
        let status = Command::new("touch")
            .arg("-r")
            .arg(src.join("f.dat"))
            .arg(basis.join("f.dat"))
            .status()
            .expect("spawn touch");
        assert!(status.success(), "touch -r failed");
        None
    }) else {
        println!("SKIP: no loopback port, or a scratch path with a space");
        return;
    };
    let out = served.push(&[
        "-a".into(),
        "--link-dest=../01".into(),
        format!("{}/", served.root.join("src").display()),
        format!("{}/", served.url(&served.root.join("bak").join("00"))),
    ]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "push failed\nstderr:\n{stderr}");
    let dest = fs::metadata(served.root.join("bak/00/f.dat")).expect("stat dest");
    let basis = fs::metadata(served.root.join("bak/01/f.dat")).expect("stat basis");
    assert_eq!(
        (dest.dev(), dest.ino()),
        (basis.dev(), basis.ino()),
        "--link-dest=../01 was ignored on a served `path = /` module: the file \
         was re-sent instead of hard-linked\nstderr:\n{stderr}\nlog:\n{}",
        served.log(),
    );
}
