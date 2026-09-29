//! `--copy-unsafe-links` must reach an rsh SERVER SENDER.
//!
//! On a pull the remote side builds the file list, so it is the side that
//! decides whether an unsafe symlink travels as a link or as its referent.
//! upstream: options.c:3075-3076 forwards `--copy-unsafe-links` to the server,
//! and flist.c:492 (`readlink_stat()`) makes that server dereference every
//! symlink whose target is absolute or climbs out with `..`. A server that
//! drops the flag delivers those links verbatim with exit 0, so a client that
//! asked for an escape-free copy silently receives links pointing outside the
//! transferred tree.
//!
//! A dangling unsafe link cannot be dereferenced. upstream: flist.c:1676-1681
//! reports it as `symlink has no referent` under FERROR_XFER and sets
//! IOERR_GENERAL, so the run exits 23 rather than treating it as a vanished
//! file (24).

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::Path;

use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// Builds `root/src` holding a regular file, a safe relative link, an
/// absolute link and a `..` link, both pointing at `root/outside/target.txt`.
fn unsafe_link_fixture(root: &Path) {
    let src = root.join("src");
    let outside = root.join("outside");
    fs::create_dir_all(&src).expect("mkdir src");
    fs::create_dir_all(&outside).expect("mkdir outside");
    fs::write(outside.join("target.txt"), b"outside-secret\n").expect("write target");
    fs::write(src.join("plain.txt"), b"plain\n").expect("write plain");
    symlink("plain.txt", src.join("safe-link")).expect("safe link");
    symlink(outside.join("target.txt"), src.join("abs-link")).expect("abs link");
    symlink("../outside/target.txt", src.join("dotdot-link")).expect("dotdot link");
}

fn pull(root: &Path, dest: &Path) -> test_support::CliOutput {
    let stub = LshRunnerStub::locate().expect("lsh-stub located");
    OcRsyncCliRunner::new()
        .arg("-a")
        .arg("--copy-unsafe-links")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", root.join("src").display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("pull run")
}

fn assert_dereferenced(dest: &Path, name: &str) {
    let path = dest.join(name);
    let meta = fs::symlink_metadata(&path).expect("entry delivered");
    assert!(
        meta.file_type().is_file(),
        "{name}: an unsafe symlink must arrive as its referent, got {:?}",
        meta.file_type()
    );
    assert_eq!(fs::read(&path).expect("read"), b"outside-secret\n");
}

#[test]
fn rsh_pull_dereferences_absolute_and_dotdot_symlinks() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    unsafe_link_fixture(tmp.path());
    let dest = tmp.path().join("dst");

    pull(tmp.path(), &dest).assert_success();

    assert_dereferenced(&dest, "abs-link");
    assert_dereferenced(&dest, "dotdot-link");
    // A link that stays inside the tree is not unsafe and keeps its form.
    assert_eq!(
        fs::read_link(dest.join("safe-link")).expect("safe link kept"),
        Path::new("plain.txt")
    );
}

#[test]
fn rsh_pull_dangling_unsafe_symlink_exits_23_with_no_referent() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    unsafe_link_fixture(tmp.path());
    symlink("../outside/missing.txt", tmp.path().join("src/dangle")).expect("dangling link");
    let dest = tmp.path().join("dst");

    let out = pull(tmp.path(), &dest);
    out.assert_exit(23);

    let expected = format!(
        "symlink has no referent: \"{}\"",
        tmp.path().join("src/dangle").display()
    );
    assert!(
        out.stderr_contains(&expected),
        "stderr must carry upstream's message {expected:?}, got:\n{}",
        out.stderr_str()
    );
    assert!(
        fs::symlink_metadata(dest.join("dangle")).is_err(),
        "a dangling unsafe link must not be delivered in any form"
    );
    // The rest of the tree still transfers.
    assert_dereferenced(&dest, "abs-link");
}
