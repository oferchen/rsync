//! A nested unix socket must not fail the transfer.
//!
//! Upstream refuses to create a socket under a parent it would have to
//! re-resolve, on the platforms with no `bindat(2)` - the BSDs, macOS and
//! Solaris (`syscall.c:1369-1378`, `do_mknod_at()`'s socket arm sets
//! `EOPNOTSUPP` whenever `dfd != AT_FDCWD`). The generator then SKIPS the
//! entry with a warning instead of turning it into a transfer error
//! (`generator.c:2506-2521`): a socket inode is only a placeholder, so a live
//! socket is not usefully transferred and losing one must not cost the rest of
//! the tree.
//!
//! oc instead bound the socket through an unconfined path-based `bind(2)` on
//! the absolute destination. Two consequences, both measured against the real
//! rsync 3.5.0 binary on macOS:
//!
//! * a nested socket was created where upstream deliberately refuses - the
//!   race-safety divergence;
//! * and where the absolute path exceeded `sun_path` (104 bytes on Apple) the
//!   bind failed `ENAMETOOLONG` and oc exited 23, losing the whole transfer,
//!   while upstream exited 0 with the regular files in place.
//!
//! ## What these cells assert, and on which platform
//!
//! The transfer outcome is platform-independent and is the substance: exit 0
//! with every regular file present. The SKIP itself is only observable where
//! the platform has the refusal, so that half is `cfg`-gated to exactly the
//! arm the fix targets - mirroring upstream's own cell, whose comment says
//! "On Linux the socket is created; either way the transfer exits 0".
//!
//! The FIFO cell is the non-vacuity companion: without it a blanket
//! "skip every special file" regression would satisfy every other assertion
//! here.

mod integration;

// Every item below that names these is `cfg(unix)`, so on Windows the import
// has no consumer and `-D warnings` rejects it. Gate it on exactly the same
// predicate its users carry, matching tests/integration_links.rs.
#[cfg(unix)]
use integration::helpers::{RsyncCommand, TestDir};

/// Binds a unix socket at `dir/<rel>` using a short relative name, so the
/// FIXTURE never trips `sun_path` itself - only the code under test can.
#[cfg(unix)]
fn bind_socket(dir: &TestDir, rel: &str) {
    use std::os::unix::fs::FileTypeExt;
    use std::os::unix::net::UnixListener;

    let path = dir.path().join(rel);
    let parent = path.parent().expect("socket has a parent").to_path_buf();
    let name = path.file_name().expect("socket has a name").to_owned();

    let previous = std::env::current_dir().expect("cwd");
    std::env::set_current_dir(&parent).expect("chdir to the socket's parent");
    let listener = UnixListener::bind(&name);
    std::env::set_current_dir(previous).expect("restore cwd");

    listener.expect("bind the fixture socket");
    assert!(
        std::fs::symlink_metadata(&path)
            .expect("fixture socket metadata")
            .file_type()
            .is_socket(),
        "the fixture must really be a socket, or every assertion below is vacuous"
    );
}

#[cfg(unix)]
fn seed(dir: &TestDir) {
    dir.mkdir("src").expect("src");
    dir.mkdir("src/sub").expect("src/sub");
    dir.mkdir("dest").expect("dest");
    dir.write_file("src/sub/f.txt", b"regular\n")
        .expect("f.txt");
    dir.write_file("src/top.txt", b"top\n").expect("top.txt");
}

#[cfg(unix)]
fn copy_tree(dir: &TestDir) -> std::process::Output {
    RsyncCommand::new()
        .arg("-a")
        .arg(format!("{}/", dir.path().join("src").display()))
        .arg(format!("{}/", dir.path().join("dest").display()))
        .assert_success()
}

/// `-a` implies `--specials`, so a nested socket is offered for creation. The
/// transfer must still exit 0 and land every regular file, whether the
/// platform creates the socket or refuses it.
#[cfg(unix)]
#[test]
fn a_nested_socket_does_not_fail_the_transfer() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    bind_socket(&dir, "src/sub/thesock");

    copy_tree(&dir);

    assert!(
        dir.exists("dest/sub/f.txt"),
        "a regular file was lost alongside the nested socket"
    );
    assert!(
        dir.exists("dest/top.txt"),
        "a regular file was lost alongside the nested socket"
    );
}

/// Where the platform has no `bindat(2)` the entry is skipped, and the notice
/// carries upstream's wording so an operator can tell a skip from a silent
/// omission.
#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
#[test]
fn a_nested_socket_is_skipped_with_upstreams_notice() {
    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    bind_socket(&dir, "src/sub/thesock");

    let output = copy_tree(&dir);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(
        stderr.contains("skipping socket (creation unsupported here):"),
        "the skip must be announced, not silent; stderr was: {stderr}"
    );
    assert!(
        !dir.exists("dest/sub/thesock"),
        "a nested socket must not be created through an unconfined path-based \
         bind where upstream refuses it"
    );
}

/// The non-vacuity companion: the skip is keyed on the node being a SOCKET, so
/// a nested FIFO - which `mkfifo(2)` can create race-safely - must still be
/// materialised. Without this cell, skipping every special file would satisfy
/// the assertions above.
#[cfg(unix)]
#[test]
fn a_nested_fifo_is_still_created() {
    use std::os::unix::fs::FileTypeExt;

    let dir = TestDir::new().expect("scratch dir");
    seed(&dir);
    let fifo = dir.path().join("src/sub/thefifo");
    let made = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .expect("run mkfifo");
    assert!(made.success(), "mkfifo failed for the fixture");
    assert!(
        std::fs::symlink_metadata(&fifo)
            .expect("fixture fifo metadata")
            .file_type()
            .is_fifo(),
        "the fixture must really be a FIFO, or this cell proves nothing"
    );

    copy_tree(&dir);

    let created = std::fs::symlink_metadata(dir.path().join("dest/sub/thefifo"))
        .expect("the nested FIFO must be created");
    assert!(
        created.file_type().is_fifo(),
        "the destination entry must be a FIFO, not a placeholder"
    );
}
