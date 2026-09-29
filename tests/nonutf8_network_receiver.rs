//! A network receiver must keep names that are not valid UTF-8 byte-exact.
//!
//! Upstream never decodes a file name: `flist.c:3295` compares raw bytes in
//! `flist_sort_and_clean()`. oc's receiver compared lossy names in that
//! duplicate pass, so distinct non-UTF-8 names looked equal and all but one
//! were dropped with exit code 0.
//!
//! Both directions run the receiver in oc: a PUSH puts it in the `--server`
//! process, a PULL in the local client.
//!
//! Linux only: macOS (APFS) refuses names that are not valid UTF-8, and the
//! fixture needs them on disk.

#![cfg(target_os = "linux")]

use std::ffi::OsStr;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// `(relative path bytes, contents)`. Two files differ only before a shared
/// invalid byte.
const FILES: &[(&[u8], &str)] = &[
    (b"f\xef", "one\n"),
    (b"g\xef", "two\n"),
    (b"ok", "plain\n"),
];

fn oc_rsync_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"))
}

fn rel(bytes: &[u8]) -> &Path {
    Path::new(OsStr::from_bytes(bytes))
}

/// Drops the rsh options and the host, then runs the remote command locally.
fn write_rsh_shim(dir: &Path) -> PathBuf {
    let script = dir.join("fake_rsh.sh");
    fs::write(
        &script,
        "#!/bin/sh\n\
         while [ $# -gt 0 ]; do\n\
         case \"$1\" in\n\
         -*) shift ;;\n\
         *) break ;;\n\
         esac\n\
         done\n\
         shift || true\n\
         exec \"$@\"\n",
    )
    .expect("write rsh shim");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod shim");
    script
}

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    shim: PathBuf,
}

fn fixture() -> Fixture {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path().to_path_buf();
    let src = root.join("src");
    fs::create_dir_all(&src).expect("create src");
    fs::create_dir_all(root.join("dst")).expect("create dst");
    for (name, body) in FILES {
        let path = src.join(rel(name));
        fs::create_dir_all(path.parent().expect("parent")).expect("create parent");
        fs::write(&path, body).expect("write source file");
    }
    let shim = write_rsh_shim(&root);
    Fixture {
        _temp: temp,
        root,
        shim,
    }
}

fn run(fx: &Fixture, source: String, dest: String) -> (Option<i32>, String) {
    let binary = oc_rsync_binary();
    let out = test_support::OcRsyncCliRunner::new()
        .binary(&binary)
        .arg("-a")
        .arg("--rsh")
        .arg(&fx.shim)
        .arg("--rsync-path")
        .arg(&binary)
        .arg(source)
        .arg(dest)
        .run()
        .expect("transfer did not finish");
    (
        out.status,
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_all_received(fx: &Fixture, status: Option<i32>, stderr: &str) {
    let dst = fx.root.join("dst");
    for (name, body) in FILES {
        let path = dst.join(rel(name));
        assert_eq!(
            fs::read_to_string(&path).ok().as_deref(),
            Some(*body),
            "{} must arrive byte-exact; stderr was: {stderr}",
            path.display()
        );
    }
    assert_eq!(count_files(&dst), FILES.len(), "stderr was: {stderr}");
    assert_eq!(status, Some(0), "stderr was: {stderr}");
}

fn count_files(dir: &Path) -> usize {
    fs::read_dir(dir)
        .expect("read dir")
        .map(|e| e.expect("dir entry").path())
        .map(|p| if p.is_dir() { count_files(&p) } else { 1 })
        .sum()
}

#[test]
fn push_to_oc_server_keeps_non_utf8_names() {
    let fx = fixture();
    let (status, stderr) = run(
        &fx,
        format!("{}/src/", fx.root.display()),
        format!("host:{}/dst/", fx.root.display()),
    );
    assert_all_received(&fx, status, &stderr);
}

#[test]
fn pull_by_oc_client_keeps_non_utf8_names() {
    let fx = fixture();
    let (status, stderr) = run(
        &fx,
        format!("host:{}/src/", fx.root.display()),
        format!("{}/dst/", fx.root.display()),
    );
    assert_all_received(&fx, status, &stderr);
}
