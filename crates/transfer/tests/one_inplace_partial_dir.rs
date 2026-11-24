//! A network receiver resuming from a `--partial-dir` file rewrites that file
//! in place and renames it onto the destination, as rsync 3.5.2 does under the
//! negotiated `CF_INPLACE_PARTIAL_DIR` capability (`one_inplace`), and it opens
//! the file through the ownership walk.
//!
//! The walk is the security half. A partial dir reached through a symlink owned
//! by neither root nor the receiver's euid is refused with upstream's
//! diagnostic and the file is not transferred (exit 23); the receiver never
//! writes through an attacker's symlink. Before this, the receiver wrote a temp
//! file next to the destination and never opened the partial-dir path for
//! writing, so it never checked it.
//!
//! The rewrite-in-place half is observable without privilege: the destination
//! keeps the partial file's inode. A temp+rename receiver gives it a new one.
//! That half runs everywhere, on the push, pull and daemon legs. Planting a
//! symlink owned by another uid needs root, so the refusal cells run only as
//! root and print the skip otherwise, like the repo's other untrusted-owner
//! cells.
//!
//! Verified against rsync 3.5.2 in a user-namespace root fixture: with the
//! partial dir a symlink owned by uid 65534, push, pull and daemon all exit 23
//! with the refusal line and `open "<partial>" failed`, leave the partial file
//! untouched and create no destination file. With the symlink owned by root,
//! all three exit 0 and consume the partial file.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.2/compat.c:747`, `:798-799` - the `I` capability sets
//!   `inplace_partial`.
//! - `rsync-3.5.2/receiver.c:1153-1155` - `one_inplace`.
//! - `rsync-3.5.2/receiver.c:1212-1244` - the output is `partialptr`, opened with
//!   `secure_recv_open(..., one_inplace)`; a failure prints `open %s failed`.
//! - `rsync-3.5.2/receiver.c:202-210` - `secure_recv_open()` turns on the
//!   ownership walk.
//! - `rsync-3.5.2/syscall.c:560-565` - the walk's refusal and its diagnostic.
//! - `rsync-3.5.2/receiver.c:1302-1316` - `finish_transfer()` renames the
//!   partial file onto the destination.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, lchown, symlink};
use std::path::Path;

use test_support::oc_daemon::OcDaemon;
use test_support::{
    CliOutput, LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// uid 65534 is `nobody` on every supported Unix.
const UNTRUSTED_UID: u32 = 65534;

/// Size of the basis: several blocks, so the delta has matches to reuse.
const BASIS_LEN: usize = 256 * 1024;

#[derive(Clone, Copy, Debug)]
enum Leg {
    Push,
    Pull,
    Daemon,
}

/// How the partial dir `dst/pd` is reached.
#[derive(Clone, Copy, Debug)]
enum PartialDir {
    /// A plain directory.
    Directory,
    /// A symlink to `dst/real`, owned by the test's own uid (trusted).
    TrustedSymlink,
    /// A symlink to `dst/real`, owned by [`UNTRUSTED_UID`].
    UntrustedSymlink,
}

fn basis() -> Vec<u8> {
    (0..BASIS_LEN).map(|i| (i * 7 % 251) as u8).collect()
}

/// Seeds `src/f` (the basis, shortened, with one byte changed) and the partial-dir copy of
/// the basis. The destination file itself does not exist, so the partial file
/// is the only basis the generator can pick. Returns the partial file's path.
fn fixture(root: &Path, partial: PartialDir) -> std::path::PathBuf {
    let basis = basis();
    // Shorter than the partial file, so a rewrite in place must also cut it.
    let mut source = basis[..BASIS_LEN - 3000].to_vec();
    source[1000] ^= 0xff;
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src/f"), &source).expect("write source");
    let dst = root.join("dst");
    let partial_dir = match partial {
        PartialDir::Directory => dst.join("pd"),
        PartialDir::TrustedSymlink | PartialDir::UntrustedSymlink => {
            fs::create_dir_all(dst.join("real")).expect("mkdir real");
            symlink("real", dst.join("pd")).expect("plant pd");
            dst.join("real")
        }
    };
    fs::create_dir_all(&partial_dir).expect("mkdir partial dir");
    fs::write(partial_dir.join("f"), &basis).expect("write partial file");
    if let PartialDir::UntrustedSymlink = partial {
        lchown(dst.join("pd"), Some(UNTRUSTED_UID), Some(UNTRUSTED_UID)).expect("lchown pd");
    }
    partial_dir.join("f")
}

fn daemon_config(root: &Path) -> std::path::PathBuf {
    let config = root.join("rsyncd.conf");
    fs::write(
        &config,
        format!(
            "pid file = {pid}\nuse chroot = false\n{ids}\n[data]\npath = {path}\nread only = false\n",
            pid = root.join("rsyncd.pid").display(),
            path = root.join("dst").display(),
            // A root daemon would otherwise drop to `nobody` (clientserver.c:837),
            // whose euid then owns - and trusts - the planted symlink.
            ids = if rustix::process::geteuid().is_root() {
                "uid = 0\ngid = 0\n"
            } else {
                ""
            },
        ),
    )
    .expect("write daemon config");
    config
}

fn run(root: &Path, leg: Leg) -> CliOutput {
    run_with(root, leg, &[])
}

fn run_with(root: &Path, leg: Leg, extra: &[&str]) -> CliOutput {
    let src = format!("{}/", root.join("src").display());
    let dst = format!("{}/", root.join("dst").display());
    let runner = OcRsyncCliRunner::new()
        .arg("-a")
        .arg("--no-whole-file")
        .arg("--partial-dir=pd")
        .args(extra);
    match leg {
        Leg::Push | Leg::Pull => {
            let stub = LshRunnerStub::locate().expect("lsh-stub located");
            let (src, dst) = match leg {
                Leg::Push => (src, format!("localhost:{dst}")),
                _ => (format!("localhost:{src}"), dst),
            };
            runner
                .arg(format!("--rsh={}", stub.path().display()))
                .arg(format!(
                    "--rsync-path={}",
                    test_support::oc_rsync_bin().display()
                ))
                .arg(src)
                .arg(dst)
                .run()
                .expect("transfer run")
        }
        Leg::Daemon => {
            let daemon = OcDaemon::builder(daemon_config(root))
                .spawn()
                .expect("start the daemon");
            let out = runner
                .arg(src)
                .arg(format!("rsync://127.0.0.1:{}/data/", daemon.port()))
                .run()
                .expect("transfer run");
            drop(daemon);
            out
        }
    }
}

/// WHY: under `one_inplace` the receiver rewrites the partial file and renames
/// it onto the destination, so the destination keeps the partial file's inode.
/// A temp+rename receiver never opens the partial-dir path for writing, so it
/// cannot apply upstream's ownership walk to it.
fn resumes_in_place(leg: Leg, partial: PartialDir) {
    let tmp = create_tempdir();
    let root = fs::canonicalize(tmp.path()).expect("canonicalize");
    let partial_file = fixture(&root, partial);
    let partial_inode = fs::metadata(&partial_file).expect("partial").ino();

    let out = run(&root, leg);
    out.assert_exit(0);
    let dest = root.join("dst/f");
    assert_eq!(
        fs::read(&dest).expect("destination"),
        fs::read(root.join("src/f")).expect("source"),
        "{leg:?} {partial:?}: wrong destination content"
    );
    assert_eq!(
        fs::metadata(&dest).expect("destination").ino(),
        partial_inode,
        "{leg:?} {partial:?}: the destination is not the rewritten partial file"
    );
    assert!(
        !partial_file.exists(),
        "{leg:?} {partial:?}: the partial file was not consumed"
    );
}

/// WHY: a partial dir reached through a symlink owned by another uid is an
/// attacker's plant. 3.5.2 refuses to follow it, prints the walk's diagnostic
/// and an open failure, transfers nothing and exits 23.
fn refuses_untrusted_partial_dir(leg: Leg) {
    if !rustix::process::geteuid().is_root() {
        eprintln!("skipped: planting a symlink owned by another uid needs root");
        return;
    }
    let tmp = create_tempdir();
    let root = fs::canonicalize(tmp.path()).expect("canonicalize");
    let partial_file = fixture(&root, PartialDir::UntrustedSymlink);

    let out = run(&root, leg);
    out.assert_exit(23);
    let stderr = out.stderr_str();
    assert!(
        stderr.contains(fast_io::owner_walk::UNTRUSTED_SYMLINK_REFUSAL),
        "{leg:?}: no ownership refusal:\n{stderr}"
    );
    let open_line = stderr
        .lines()
        .find(|line| line.contains("open \"") && line.contains("failed"))
        .unwrap_or_else(|| panic!("{leg:?}: no open failure:\n{stderr}"));
    assert!(
        open_line.contains("pd/f\""),
        "{leg:?}: the open failure must name the partial file: {open_line}"
    );
    assert!(
        !root.join("dst/f").exists(),
        "{leg:?}: the file was transferred through the refused partial dir"
    );
    assert_eq!(
        fs::read(&partial_file).expect("partial file"),
        basis(),
        "{leg:?}: the receiver wrote through the untrusted symlink"
    );
}

/// CONTROL. `--insecure-links` opts a pull's receiver out of the walk, so
/// 3.5.2 follows the same symlink and transfers (`syscall.c:126-127`); the
/// refusal above is the walk, not an unconditional block.
#[test]
fn pull_follows_an_untrusted_partial_dir_symlink_under_insecure_links() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    if !rustix::process::geteuid().is_root() {
        eprintln!("skipped: planting a symlink owned by another uid needs root");
        return;
    }
    let tmp = create_tempdir();
    let root = fs::canonicalize(tmp.path()).expect("canonicalize");
    fixture(&root, PartialDir::UntrustedSymlink);

    run_with(&root, Leg::Pull, &["--insecure-links"]).assert_exit(0);
    assert_eq!(
        fs::read(root.join("dst/f")).expect("destination"),
        fs::read(root.join("src/f")).expect("source")
    );
}

#[test]
fn push_resumes_a_partial_dir_file_in_place() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    resumes_in_place(Leg::Push, PartialDir::Directory);
    resumes_in_place(Leg::Push, PartialDir::TrustedSymlink);
}

#[test]
fn pull_resumes_a_partial_dir_file_in_place() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    resumes_in_place(Leg::Pull, PartialDir::Directory);
    resumes_in_place(Leg::Pull, PartialDir::TrustedSymlink);
}

#[test]
fn daemon_push_resumes_a_partial_dir_file_in_place() {
    require_binaries!("oc-rsync");
    resumes_in_place(Leg::Daemon, PartialDir::Directory);
    resumes_in_place(Leg::Daemon, PartialDir::TrustedSymlink);
}

#[test]
fn push_refuses_a_partial_dir_symlink_owned_by_another_user() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    refuses_untrusted_partial_dir(Leg::Push);
}

#[test]
fn pull_refuses_a_partial_dir_symlink_owned_by_another_user() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    refuses_untrusted_partial_dir(Leg::Pull);
}

#[test]
fn daemon_push_refuses_a_partial_dir_symlink_owned_by_another_user() {
    require_binaries!("oc-rsync");
    refuses_untrusted_partial_dir(Leg::Daemon);
}
