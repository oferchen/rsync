//! The ownership walk used for operator-supplied paths.
//!
//! The refusal arm - a symlink component owned by neither uid 0 nor our euid -
//! needs a second uid to plant, so it is covered by the root leg of the upstream
//! 3.5.0 testsuite (`backup-dir-symlink-race`) rather than duplicated here with
//! a privilege check that would pass vacuously for an unprivileged runner.
//! What this file pins is everything reachable as an ordinary user: that the
//! walk still *works* (it must not become a blanket refusal), and that its two
//! non-obvious rules - absolute targets restart at `/`, and the hop budget is
//! finite - hold.
//!
//! upstream: `rsync-3.5.0/syscall.c:286` `ona_open()`.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use tempfile::TempDir;

/// Resolve `path` and read back the leaf through the returned parent handle,
/// proving the descriptor really is the directory holding it.
fn resolve_and_read(path: &Path) -> std::io::Result<String> {
    let (dirfd, leaf) = fast_io::owner_trusted_parent(path)?;
    let mut buf = Vec::new();
    let file = rustix::fs::openat(
        &dirfd,
        leaf.as_os_str(),
        rustix::fs::OFlags::RDONLY,
        rustix::fs::Mode::empty(),
    )
    .map_err(|errno| std::io::Error::from_raw_os_error(errno.raw_os_error()))?;
    std::io::Read::read_to_end(&mut std::fs::File::from(file), &mut buf)?;
    Ok(String::from_utf8(buf).expect("utf-8 fixture"))
}

/// Non-vacuity companion: with no symlink in play the walk is an ordinary
/// resolution. Without this, every refusal assertion below would also hold if
/// the walk simply failed for all inputs.
#[test]
fn the_walk_resolves_a_plain_nested_path() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir_all(root.path().join("a/b")).expect("mkdir");
    fs::write(root.path().join("a/b/f"), "payload").expect("write");

    assert_eq!(
        resolve_and_read(&root.path().join("a/b/f")).expect("resolve"),
        "payload"
    );
}

/// A symlink component we own is the operator's own layout, so it is followed -
/// upstream refuses on ownership, not on the mere presence of a link.
#[test]
fn a_symlink_component_we_own_is_followed() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("real")).expect("mkdir real");
    fs::write(root.path().join("real/f"), "payload").expect("write");
    symlink("real", root.path().join("link")).expect("plant symlink");

    assert_eq!(
        resolve_and_read(&root.path().join("link/f")).expect("resolve"),
        "payload"
    );
}

/// An absolute symlink target restarts the walk at `/` rather than being
/// appended to the current position.
///
/// upstream: `rsync-3.5.0/syscall.c:422`.
#[test]
fn an_absolute_symlink_target_restarts_the_walk_at_the_root() {
    let root = TempDir::new().expect("tempdir");
    let elsewhere = root.path().join("elsewhere");
    fs::create_dir(&elsewhere).expect("mkdir elsewhere");
    fs::write(elsewhere.join("f"), "payload").expect("write");
    fs::create_dir(root.path().join("deep")).expect("mkdir deep");
    // An absolute target from a nested position: appending instead of
    // restarting would look for `<root>/deep/<root>/elsewhere` and fail.
    symlink(&elsewhere, root.path().join("deep/hop")).expect("plant symlink");

    assert_eq!(
        resolve_and_read(&root.path().join("deep/hop/f")).expect("resolve"),
        "payload"
    );
}

/// The hop budget is finite, so a symlink cycle terminates in a refusal instead
/// of spinning. The refusal is `ELOOP` and deliberately not `EXDEV`: callers
/// treat `EXDEV` as cross-device and fall back to copy+remove, which would
/// launder the refusal.
///
/// upstream: `rsync-3.5.0/syscall.c:361` `int loops = 40;`.
#[test]
fn a_symlink_cycle_is_refused_with_eloop() {
    let root = TempDir::new().expect("tempdir");
    symlink("b", root.path().join("a")).expect("plant a");
    symlink("a", root.path().join("b")).expect("plant b");

    let err = fast_io::owner_trusted_parent(&root.path().join("a/f"))
        .expect_err("a symlink cycle must not resolve");

    assert_eq!(
        err.raw_os_error(),
        Some(libc::ELOOP),
        "refusal must be ELOOP"
    );
}

/// `operator_rename` reaches a destination behind a trusted symlink - the
/// out-of-tree `--backup-dir` shape the confined resolver would wrongly refuse.
#[test]
fn operator_rename_commits_through_a_trusted_symlink() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("backups")).expect("mkdir backups");
    symlink("backups", root.path().join("bk")).expect("plant symlink");
    let source = root.path().join("f");
    fs::write(&source, "payload").expect("write");

    fast_io::operator_rename(&source, &root.path().join("bk/f.bak"), true).expect("rename");

    assert_eq!(
        fs::read_to_string(root.path().join("backups/f.bak")).expect("read backup"),
        "payload"
    );
    assert!(!source.exists(), "the source must have moved");
}
