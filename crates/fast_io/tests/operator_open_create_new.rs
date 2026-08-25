//! `operator_open_create_new` is the `O_EXCL` arm of the ownership walk.
//!
//! It backs the receiver's staging temp under an operator-supplied `--temp-dir`.
//! `O_EXCL` alone guards only the FINAL component, so a symlink planted at the
//! `--temp-dir` itself is resolved through before the leaf is considered - the
//! walk is what closes that, and these pin the contract the caller relies on.
//!
//! The cross-uid refusal itself needs a symlink owned by a third uid, which only
//! root can plant, so it is covered by the `operator-path-temp-dir` cell of the
//! upstream 3.5.0 testsuite (run as root in CI). What is pinned here is
//! everything that is reachable unprivileged: the create, the exclusivity the
//! caller's retry loop depends on, and the FOLLOW direction - without that last
//! one a future "refuse every symlink" simplification would look correct.
//!
//! upstream: `rsync-3.5.0/syscall.c:3379` `secure_mkstemp()`.
#![cfg(unix)]

use std::fs;
use std::io::Write;

use tempfile::TempDir;

/// Non-vacuity companion: with no symlink in play the walked create makes an
/// ordinary nested file. Without this, the refusal test below would also pass if
/// `operator_open_create_new` simply failed for every input.
#[test]
fn operator_open_create_new_creates_a_nested_file() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("tmp")).expect("mkdir tmp");
    let dest = root.path().join("tmp").join(".f0.XXXXXX");

    let mut file = fast_io::operator_open_create_new(&dest, 0o666).expect("create");
    file.write_all(b"payload").expect("write");
    drop(file);

    assert_eq!(fs::read_to_string(&dest).unwrap(), "payload");
}

/// The caller (`create_new_temp`) generates a random name and RETRIES on
/// `AlreadyExists`, which is upstream's generate-then-`O_EXCL`-retry `mkstemp`
/// loop. That contract only holds if a collision reports exactly that kind - any
/// other error would escape the retry arm and abort the transfer.
#[test]
fn operator_open_create_new_reports_already_exists_on_a_collision() {
    let root = TempDir::new().expect("tempdir");
    let dest = root.path().join("occupied");
    fs::write(&dest, b"first").expect("seed");

    let error = fast_io::operator_open_create_new(&dest, 0o666)
        .expect_err("an existing name must not be opened or truncated");

    assert_eq!(
        error.kind(),
        std::io::ErrorKind::AlreadyExists,
        "the caller's retry loop keys on AlreadyExists"
    );
    assert_eq!(
        fs::read_to_string(&dest).unwrap(),
        "first",
        "O_EXCL must leave the existing file untouched"
    );
}

/// A symlink planted at the LEAF name is refused rather than followed: `O_EXCL`
/// makes the open fail instead of writing through it. Pinned because this is the
/// half `O_EXCL` does cover, so a regression here would otherwise be invisible
/// behind the walk.
#[test]
fn operator_open_create_new_refuses_a_symlinked_leaf() {
    let root = TempDir::new().expect("tempdir");
    let outside = TempDir::new().expect("outside tempdir");
    let target = outside.path().join("victim");
    fs::write(&target, b"original").expect("seed victim");
    let dest = root.path().join(".f0.XXXXXX");
    std::os::unix::fs::symlink(&target, &dest).expect("plant leaf symlink");

    fast_io::operator_open_create_new(&dest, 0o666)
        .expect_err("a symlink at the leaf must not be followed");

    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "original",
        "nothing may be written through the planted link"
    );
}

/// FOLLOW direction: a parent symlink owned by our own euid is trusted and
/// descended (`syscall.c:406` trusts uid 0 or the euid). Without this pin, a
/// "refuse every parent symlink" change would pass the refusal tests while
/// breaking a legitimate operator `--temp-dir` that happens to be a symlink -
/// which is precisely the case upstream's comment says must keep working.
#[test]
fn operator_open_create_new_follows_a_self_owned_parent_symlink() {
    let root = TempDir::new().expect("tempdir");
    fs::create_dir(root.path().join("real")).expect("mkdir real");
    std::os::unix::fs::symlink("real", root.path().join("tmp")).expect("plant symlink");
    let dest = root.path().join("tmp").join(".f0.XXXXXX");

    let mut file =
        fast_io::operator_open_create_new(&dest, 0o666).expect("create through our own link");
    file.write_all(b"payload").expect("write");
    drop(file);

    assert_eq!(
        fs::read_to_string(root.path().join("real").join(".f0.XXXXXX")).unwrap(),
        "payload",
        "the temp must land in the symlink's target"
    );
}
