//! Unit tests for the commit rename's retry and cross-filesystem fallback.
//!
//! The rename is injected, so `EXDEV` and `ETXTBSY` are reproduced on a single
//! filesystem and each arm of upstream's `robust_rename()` is pinned directly.

use super::*;
use std::cell::RefCell;
use std::fs;
use std::os::unix::fs::PermissionsExt;

struct Fixture {
    _tmp: tempfile::TempDir,
    dest: PathBuf,
    temp: PathBuf,
}

/// A destination root and a temp file standing in an operator `--temp-dir`
/// outside it, the shape a cross-filesystem `--temp-dir` commits from.
fn fixture(content: &[u8]) -> Fixture {
    let tmp = tempfile::tempdir().expect("tempdir");
    let base = fs::canonicalize(tmp.path()).expect("canonicalize");
    let dest = base.join("dest");
    let temp_dir = base.join("tempdir");
    fs::create_dir(&dest).expect("mkdir dest");
    fs::create_dir(&temp_dir).expect("mkdir tempdir");
    let temp = temp_dir.join(".file.AbC123");
    fs::write(&temp, content).expect("write temp");
    Fixture {
        _tmp: tmp,
        dest,
        temp,
    }
}

fn unanchored(root: &Path) -> CommitAnchor<'_> {
    CommitAnchor {
        sandbox: None,
        root,
    }
}

fn os_error(errno: i32) -> io::Error {
    io::Error::from_raw_os_error(errno)
}

/// The fallback exists because a rename cannot cross a mount: without it a
/// `--temp-dir` on another filesystem fails every file. The copy must land the
/// exact bytes and permission bits at the destination and remove the temp.
#[test]
fn exdev_copies_into_place_and_removes_the_temp() {
    let fx = fixture(b"received content");
    fs::set_permissions(&fx.temp, fs::Permissions::from_mode(0o640)).unwrap();
    let final_path = fx.dest.join("file");

    let outcome = robust_rename(unanchored(&fx.dest), &fx.temp, &final_path, None, |_, _| {
        Err(os_error(libc::EXDEV))
    })
    .expect("cross-filesystem commit succeeds by copying");

    assert_eq!(outcome, Renamed::Copied(final_path.clone()));
    assert_eq!(fs::read(&final_path).unwrap(), b"received content");
    assert_eq!(
        fs::metadata(&final_path).unwrap().permissions().mode() & 0o777,
        0o640,
        "upstream copy_file() creates the copy with the file's mode",
    );
    assert!(!fx.temp.exists(), "the temp is unlinked after the copy");
}

/// upstream `unlink_and_reopen()` removes the old destination before an
/// `O_EXCL` create, so a read-only pre-existing file is replaced rather than
/// failing the copy with `EACCES`.
#[test]
fn exdev_replaces_a_read_only_destination() {
    let fx = fixture(b"new");
    let final_path = fx.dest.join("file");
    fs::write(&final_path, b"old content").unwrap();
    fs::set_permissions(&final_path, fs::Permissions::from_mode(0o444)).unwrap();

    robust_rename(unanchored(&fx.dest), &fx.temp, &final_path, None, |_, _| {
        Err(os_error(libc::EXDEV))
    })
    .expect("copy replaces the read-only destination");

    assert_eq!(fs::read(&final_path).unwrap(), b"new");
}

/// A relative `--partial-dir` keeps the destination change atomic: the copy is
/// staged in the partial dir and only a same-filesystem rename touches the
/// destination. The staged name, not the destination, must receive the copy,
/// and the emptied partial dir must not be left in the tree.
#[test]
fn exdev_with_relative_partial_dir_stages_the_copy_then_renames() {
    let fx = fixture(b"staged content");
    let final_path = fx.dest.join("file");
    let staged = fx.dest.join(".rsync-partial").join("file");
    let calls = RefCell::new(Vec::new());

    let copied = finish_rename(
        unanchored(&fx.dest),
        &fx.temp,
        &final_path,
        Some(Path::new(".rsync-partial")),
        |old, new| {
            calls
                .borrow_mut()
                .push((old.to_path_buf(), new.to_path_buf()));
            if calls.borrow().len() == 1 {
                Err(os_error(libc::EXDEV))
            } else {
                fs::rename(old, new)
            }
        },
    )
    .expect("staged commit succeeds");

    assert!(copied, "a copy tells the caller to re-apply metadata");
    assert_eq!(
        calls.into_inner(),
        vec![
            (fx.temp.clone(), final_path.clone()),
            (staged.clone(), final_path.clone()),
        ],
        "the copy lands in the partial dir and is renamed onto the destination",
    );
    assert_eq!(fs::read(&final_path).unwrap(), b"staged content");
    assert!(!fx.temp.exists());
    assert!(
        !staged.parent().unwrap().exists(),
        "the emptied relative partial dir is removed",
    );
}

/// upstream passes `temp_copy_name` only for a relative `--partial-dir`
/// (`rsync.c:889`); an absolute one may sit on yet another filesystem, so the
/// copy goes straight to the destination.
#[test]
fn absolute_partial_dir_is_not_a_staging_target() {
    let fx = fixture(b"direct");
    let final_path = fx.dest.join("file");
    let absolute_partial = fx.dest.parent().unwrap().join("partial");
    let calls = RefCell::new(0);

    let copied = finish_rename(
        unanchored(&fx.dest),
        &fx.temp,
        &final_path,
        Some(&absolute_partial),
        |_, _| {
            *calls.borrow_mut() += 1;
            Err(os_error(libc::EXDEV))
        },
    )
    .expect("direct copy succeeds");

    assert!(copied);
    assert_eq!(*calls.borrow(), 1, "no second rename from a staging name");
    assert_eq!(fs::read(&final_path).unwrap(), b"direct");
    assert!(!absolute_partial.exists());
}

/// Only `EXDEV` may trigger the copy. Any other rename failure - a permission
/// error, or a confinement refusal - must surface untouched, leaving the temp
/// in place and the destination unwritten.
#[test]
fn other_rename_errors_do_not_copy() {
    let fx = fixture(b"data");
    let final_path = fx.dest.join("file");

    let error = robust_rename(unanchored(&fx.dest), &fx.temp, &final_path, None, |_, _| {
        Err(os_error(libc::EACCES))
    })
    .expect_err("EACCES propagates");

    assert_eq!(error.raw_os_error(), Some(libc::EACCES));
    assert!(fx.temp.exists());
    assert!(!final_path.exists());
}

/// upstream retries a busy target four times, unlinking it after each failed
/// try, and then reports `ETXTBSY`. The injected rename re-plants the busy
/// target each time, as a respawned executable would.
#[test]
fn busy_target_is_unlinked_and_retried_four_times() {
    let fx = fixture(b"data");
    let final_path = fx.dest.join("file");
    let calls = RefCell::new(0);

    let error = robust_rename(
        unanchored(&fx.dest),
        &fx.temp,
        &final_path,
        None,
        |_, new| {
            *calls.borrow_mut() += 1;
            fs::write(new, b"running binary").unwrap();
            Err(os_error(libc::ETXTBSY))
        },
    )
    .expect_err("tries run out");

    assert_eq!(error.kind(), io::ErrorKind::ExecutableFileBusy);
    assert_eq!(*calls.borrow(), RENAME_TRIES);
    assert!(
        !final_path.exists(),
        "the busy target was unlinked after each try"
    );
    assert!(fx.temp.exists(), "no copy is attempted for ETXTBSY");
}

/// A busy target that cannot be unlinked ends the retries at once, as
/// upstream's `robust_unlink(to) != 0` check does.
#[test]
fn busy_target_that_cannot_be_unlinked_stops_retrying() {
    let fx = fixture(b"data");
    let final_path = fx.dest.join("file");
    let calls = RefCell::new(0);

    let error = robust_rename(unanchored(&fx.dest), &fx.temp, &final_path, None, |_, _| {
        *calls.borrow_mut() += 1;
        Err(os_error(libc::ETXTBSY))
    })
    .expect_err("missing target cannot be unlinked");

    assert_eq!(error.kind(), io::ErrorKind::ExecutableFileBusy);
    assert_eq!(*calls.borrow(), 1);
}

/// Once the busy target is gone the retried rename lands the file.
#[test]
fn busy_target_retry_succeeds() {
    let fx = fixture(b"fresh");
    let final_path = fx.dest.join("file");
    fs::write(&final_path, b"running binary").unwrap();
    let calls = RefCell::new(0);

    let outcome = robust_rename(
        unanchored(&fx.dest),
        &fx.temp,
        &final_path,
        None,
        |old, new| {
            *calls.borrow_mut() += 1;
            if *calls.borrow() == 1 {
                Err(os_error(libc::ETXTBSY))
            } else {
                fs::rename(old, new)
            }
        },
    )
    .expect("second try succeeds");

    assert_eq!(outcome, Renamed::Moved);
    assert_eq!(fs::read(&final_path).unwrap(), b"fresh");
}

/// The copy must stay as confined as the rename it replaces. On Linux a
/// `RESOLVE_BENEATH` refusal is itself reported as `EXDEV`, so a copy that
/// re-resolved the destination by plain path would follow an interior symlink
/// out of the tree and launder the refusal. The copy resolves the destination
/// through the same confined walk, so it refuses too and nothing lands outside.
#[cfg(target_os = "linux")]
#[test]
fn exdev_copy_refuses_an_interior_symlink_escape() {
    if !fast_io::openat2_supported() {
        return;
    }
    let fx = fixture(b"payload");
    let outside = fx.dest.parent().unwrap().join("outside");
    fs::create_dir(&outside).unwrap();
    let sandbox = fast_io::DirSandbox::open_root(&fx.dest).expect("open sandbox");
    std::os::unix::fs::symlink(&outside, fx.dest.join("sub")).unwrap();
    let final_path = fx.dest.join("sub").join("file");

    let anchor = CommitAnchor {
        sandbox: Some(&sandbox),
        root: &fx.dest,
    };
    let result = robust_rename(anchor, &fx.temp, &final_path, None, |_, _| {
        Err(os_error(libc::EXDEV))
    });

    assert!(result.is_err(), "the confined copy refuses the escape");
    assert!(
        !outside.join("file").exists(),
        "nothing lands outside the tree"
    );
    assert!(fx.temp.exists(), "the temp is kept when the copy fails");
}
