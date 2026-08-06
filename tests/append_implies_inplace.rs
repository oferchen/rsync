//! `--append` must imply `--inplace` (upstream `options.c:2400-2411`).
//!
//! The promotion is not cosmetic. Upstream never branches on `append_mode`
//! after option parsing; it branches on the implied `inplace` flag, and the
//! two most consequential branches are about what happens to the DESTINATION:
//!
//! - `generator.c:1862,1898` - under `inplace`, `--backup` COPIES the pre-image
//!   aside and leaves the destination inode alone, because that inode is the
//!   basis the append is about to extend. Without the flag the generator takes
//!   the non-inplace path and moves the destination away (upstream `rsync.c:740`
//!   hard-links it into the backup area first), which both destroys the basis
//!   and leaves the "backup" aliasing the file that is still being written.
//! - `receiver.c:1029,1074` - `|| inplace` is what RETAINS a file whose
//!   verification failed instead of discarding it, and what makes the warning
//!   say "retained" rather than "discarded".
//!
//! These tests pin the observable end of that rule so a later change cannot
//! silently drop the promotion and reach the old behaviour again.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

/// Writes a 16-byte source and an 8-byte destination sharing the same prefix,
/// so `--append` has a real prefix to keep and a real tail to send.
fn seed(root: &Path) -> (PathBuf, PathBuf) {
    let src = root.join("src");
    let dest = root.join("dest");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(&dest).expect("create dest");
    fs::write(src.join("f.txt"), b"AAAABBBBCCCCDDDD").expect("write source");
    fs::write(dest.join("f.txt"), b"AAAABBBB").expect("write destination");
    (src, dest)
}

#[cfg(unix)]
fn inode(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).expect("stat").ino()
}

fn run(binary: &Path, args: &[&std::ffi::OsStr]) -> std::process::Output {
    Command::new(binary)
        .args(args)
        .output()
        .expect("run oc-rsync")
}

/// `--append --backup` must keep the pre-image in the backup file.
///
/// This is the case that separates "append happens to write in place" from
/// "append IS inplace". Both paths append to the same inode, so the file
/// content alone cannot tell them apart - only the backup does. Upstream
/// (`generator.c:1862,1898`) copies the pre-image aside under `inplace`; the
/// non-inplace path hard-links the destination into the backup area first
/// (`rsync.c:740`), so the "backup" grows along with the file being appended
/// to and preserves nothing.
#[test]
fn append_backup_preserves_the_pre_image() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, dest) = seed(temp.path());

    let output = run(
        &binary,
        &[
            "--append".as_ref(),
            "--backup".as_ref(),
            src.join("f.txt").as_os_str(),
            dest.as_os_str(),
        ],
    );
    assert!(
        output.status.success(),
        "--append --backup must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        fs::read(dest.join("f.txt")).expect("read destination"),
        b"AAAABBBBCCCCDDDD",
        "the appended destination must hold the full source content"
    );
    assert_eq!(
        fs::read(dest.join("f.txt~")).expect("read backup"),
        b"AAAABBBB",
        "the backup must hold the PRE-image; holding the post-image means the \
         backup was hard-linked to the destination before the append, which is \
         the non-inplace path (--append did not imply --inplace)"
    );
    #[cfg(unix)]
    assert_ne!(
        inode(&dest.join("f.txt~")),
        inode(&dest.join("f.txt")),
        "the backup must be an independent copy (generator.c:1898 copy_file); \
         a shared inode is the rsync.c:740 hard-link path, under which the \
         append grows the backup along with the destination"
    );
}

/// The same rule with `--backup-dir`: `get_backup_name()` honours it, so the
/// pre-image copy must land there instead of beside the destination.
#[test]
fn append_backup_dir_preserves_the_pre_image() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, dest) = seed(temp.path());
    let backup_dir = temp.path().join("bd");
    fs::create_dir_all(&backup_dir).expect("create backup dir");

    let backup_arg = {
        let mut arg = std::ffi::OsString::from("--backup-dir=");
        arg.push(&backup_dir);
        arg
    };
    let output = run(
        &binary,
        &[
            "--append".as_ref(),
            "--backup".as_ref(),
            backup_arg.as_os_str(),
            src.join("f.txt").as_os_str(),
            dest.as_os_str(),
        ],
    );
    assert!(
        output.status.success(),
        "--append --backup --backup-dir must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );

    assert_eq!(
        fs::read(dest.join("f.txt")).expect("read destination"),
        b"AAAABBBBCCCCDDDD"
    );
    assert_eq!(
        fs::read(backup_dir.join("f.txt")).expect("read backup-dir copy"),
        b"AAAABBBB",
        "the backup-dir copy must hold the PRE-image"
    );
}

/// Control: an explicit `--inplace --backup` was always correct. Pinning it
/// beside the `--append` case keeps the two honest - if this one ever breaks,
/// the defect is in the backup path itself rather than in the implication.
#[test]
fn explicit_inplace_backup_preserves_the_pre_image() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, dest) = seed(temp.path());

    let output = run(
        &binary,
        &[
            "--inplace".as_ref(),
            "--backup".as_ref(),
            src.join("f.txt").as_os_str(),
            dest.as_os_str(),
        ],
    );
    assert!(output.status.success(), "--inplace --backup must exit 0");
    assert_eq!(
        fs::read(dest.join("f.txt~")).expect("read backup"),
        b"AAAABBBB"
    );
}

/// `--append` rewrites the live destination, never a temp file that is renamed
/// over it. Upstream selects the write target with `if (inplace || one_inplace)`
/// (`receiver.c:968`) and only then can `receiver.c:1029` retain a failed
/// update: there is nothing to discard because the bytes already landed on the
/// destination inode.
#[cfg(unix)]
#[test]
fn append_writes_through_the_destination_inode() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, dest) = seed(temp.path());
    let before = inode(&dest.join("f.txt"));

    let output = run(
        &binary,
        &[
            "--append".as_ref(),
            src.join("f.txt").as_os_str(),
            dest.as_os_str(),
        ],
    );
    assert!(output.status.success(), "--append must exit 0");

    assert_eq!(
        inode(&dest.join("f.txt")),
        before,
        "an inplace write keeps the destination inode; a new inode means the \
         update went through a temp file and was renamed over the destination"
    );
    assert_eq!(
        fs::read(dest.join("f.txt")).expect("read destination"),
        b"AAAABBBBCCCCDDDD"
    );
}

/// The implication must not weaken the conflicts upstream derives FROM it.
/// `options.c:2424-2432` rejects `inplace` with `--partial-dir`/`--delay-updates`
/// and names the option the user actually typed (`append_mode ? "append" :
/// "inplace"`), and `options.c:2401` rejects an explicit `--whole-file`.
#[test]
fn append_keeps_the_upstream_conflict_diagnostics() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let (src, dest) = seed(temp.path());

    for (extra, expected) in [
        (
            "--partial-dir=pd",
            "--append cannot be used with --partial-dir",
        ),
        (
            "--delay-updates",
            "--append cannot be used with --delay-updates",
        ),
        ("--whole-file", "--append cannot be used with --whole-file"),
    ] {
        let output = run(
            &binary,
            &[
                "--append".as_ref(),
                extra.as_ref(),
                src.join("f.txt").as_os_str(),
                dest.as_os_str(),
            ],
        );
        assert_eq!(
            output.status.code(),
            Some(1),
            "{extra} must be a usage error (RERR_SYNTAX)"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains(expected),
            "expected {expected:?} in stderr, got:\n{stderr}"
        );
    }
}
