//! An rsh `--server` receiver must reuse an up-to-date alt-dest file.
//!
//! upstream: generator.c:1073 `try_dests_reg()` - when a missing destination
//! has a match in a `--compare-dest` / `--copy-dest` / `--link-dest`
//! directory that passes the quick check and has unchanged attributes
//! (match level 3), compare-dest skips the file, link-dest hard-links it and
//! copy-dest copies it locally (`copy_altdest_file()`). Nothing is sent.
//!
//! Two things kept the oc `--server` receiver from getting there:
//!
//! - its Landlock ruleset allowed only the destination tree, so opening a
//!   basis under an absolute alt dir failed with `EACCES` (the stat-only
//!   quick check still saw the file, which is why compare-dest matched);
//! - a relative alt dir was opened against the server's cwd, while upstream
//!   rewrites it to `curr_dir/bdir`, the destination directory
//!   (main.c:898-911).
//!
//! Every cell pushes through a `--rsh` shim, so the receiver is the real
//! `oc-rsync --server`, and asserts the positive outcome of the reuse: a
//! link-dest result shares the alt file's inode, a copy-dest result is a
//! separate inode that the client never itemized as sent, and a
//! compare-dest match leaves no destination file at all.

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::FileTime;

const PAYLOAD: &[u8] = b"alt-dest reuse payload, identical in source and alt dir\n";

/// `CARGO_BIN_EXE_oc-rsync` is a compile-time variable; reading it at run
/// time would fall through to whatever stale binary is on disk.
fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    assert!(
        built.is_file(),
        "oc-rsync binary missing at {}; refusing to fall back to a stale build",
        built.display()
    );
    built
}

/// A `--rsh` shim that drops the host argument and execs the rest locally.
fn write_rsh_shim(root: &Path) -> PathBuf {
    let shim = root.join("rsh.sh");
    fs::write(&shim, "#!/bin/sh\nshift\nexec \"$@\"\n").expect("write rsh shim");
    fs::set_permissions(&shim, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    shim
}

/// Lays out `src/f` and an identical `alt/f` with the same mtime, so the
/// alt file passes the quick check and the attribute comparison.
fn seed(root: &Path) {
    for dir in ["src", "dst", "alt"] {
        fs::create_dir_all(root.join(dir)).expect("mkdir");
    }
    let mtime = FileTime::from_unix_time(1_577_836_800, 0);
    for file in [root.join("src/f"), root.join("alt/f")] {
        fs::write(&file, PAYLOAD).expect("write payload");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).expect("chmod payload");
        filetime::set_file_mtime(&file, mtime).expect("set mtime");
    }
}

/// Pushes `src/` to `dst/` with `-ti` and the given alt-dest option, and
/// returns the client's itemized stdout.
fn push(root: &Path, alt_option: &str) -> String {
    let binary = oc_rsync_binary();
    let output = Command::new(&binary)
        .arg("-rti")
        .arg(alt_option)
        .arg("-e")
        .arg(write_rsh_shim(root))
        .arg(format!("--rsync-path={}", binary.display()))
        .arg(format!("{}/", root.join("src").display()))
        .arg(format!("localhost:{}/", root.join("dst").display()))
        .output()
        .expect("spawn oc-rsync");
    assert!(
        output.status.success(),
        "{alt_option}: expected exit 0, got {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// The two spellings the receiver must resolve the same way: absolute, and
/// relative to the destination directory.
fn spellings(root: &Path, option: &str) -> [String; 2] {
    [
        format!("--{option}={}", root.join("alt").display()),
        format!("--{option}=../alt"),
    ]
}

#[test]
fn link_dest_hard_links_an_up_to_date_alt_file() {
    for spelling in ["abs", "rel"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        seed(root);
        let option = &spellings(root, "link-dest")[usize::from(spelling == "rel")];
        let itemized = push(root, option);

        let alt = fs::metadata(root.join("alt/f")).expect("stat alt");
        let dst = fs::metadata(root.join("dst/f")).expect("stat dst");
        assert_eq!(
            (dst.dev(), dst.ino()),
            (alt.dev(), alt.ino()),
            "{option}: upstream try_dests_reg() hard-links a level-3 match \
             (generator.c:1114-1146); the destination is a separate inode\n{itemized}",
        );
    }
}

#[test]
fn copy_dest_copies_an_up_to_date_alt_file_without_sending_it() {
    for spelling in ["abs", "rel"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        seed(root);
        let option = &spellings(root, "copy-dest")[usize::from(spelling == "rel")];
        let itemized = push(root, option);

        assert_eq!(fs::read(root.join("dst/f")).expect("read dst"), PAYLOAD);
        let alt = fs::metadata(root.join("alt/f")).expect("stat alt");
        let dst = fs::metadata(root.join("dst/f")).expect("stat dst");
        assert_ne!(
            dst.ino(),
            alt.ino(),
            "{option}: copy-dest copies, it never links"
        );
        assert!(
            !itemized.lines().any(|line| line.starts_with("<f")),
            "{option}: upstream copies a level-2/3 match locally \
             (copy_altdest_file, generator.c:1161) and sends nothing; the \
             client itemized a transfer:\n{itemized}",
        );
    }
}

#[test]
fn compare_dest_skips_an_up_to_date_alt_file() {
    for spelling in ["abs", "rel"] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = tmp.path();
        seed(root);
        let option = &spellings(root, "compare-dest")[usize::from(spelling == "rel")];
        let itemized = push(root, option);

        assert!(
            !root.join("dst/f").exists(),
            "{option}: upstream skips a level-3 compare-dest match \
             (generator.c:1149-1155); the file was transferred\n{itemized}",
        );
    }
}
