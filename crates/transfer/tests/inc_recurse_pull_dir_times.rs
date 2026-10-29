//! An incremental-recursion pull restores every directory's mtime, including
//! directories whose sub-list the receiver released mid-walk.
//!
//! upstream: generator.c:2925 `touch_up_dirs(dir_flist, -1)` walks the
//! directory list, which flist_free() never touches, so every directory is
//! re-stamped after its files landed. oc frees a finished segment's entries as
//! soon as the per-segment NDX_DONE goes out (generator.c:2700-2711), so the
//! final pass sees a released directory only through what the release kept.
#![cfg(unix)]

use std::fs;
use std::path::{Path, PathBuf};

use filetime::{FileTime, set_file_mtime};
use test_support::{
    LSH_STUB_BIN, LshRunnerStub, OcRsyncCliRunner, create_tempdir, require_binaries,
};

/// Every source directory gets this mtime, well before the files inside it
/// were written, so a directory left untouched by the final pass keeps the
/// time its last file landed instead.
const DIR_MTIME: i64 = 1_000_000_000;

/// Twelve top-level directories with a nested one each: the sender emits a
/// sub-list per directory, and the receiver releases every one but the newest
/// during the walk.
fn fixture(src: &Path) -> Vec<PathBuf> {
    let mut dirs = vec![PathBuf::new()];
    for dir in 1..=12 {
        for name in ["f1", "f2", "s/g1"] {
            let path = src.join(format!("d{dir}/{name}"));
            fs::create_dir_all(path.parent().expect("parent")).expect("mkdir src");
            fs::write(&path, format!("{dir}/{name}")).expect("write src file");
        }
        dirs.push(PathBuf::from(format!("d{dir}")));
        dirs.push(PathBuf::from(format!("d{dir}/s")));
    }
    for dir in &dirs {
        set_file_mtime(src.join(dir), FileTime::from_unix_time(DIR_MTIME, 0))
            .expect("backdate src dir");
    }
    dirs
}

#[test]
fn pull_restores_mtime_of_every_released_segments_directory() {
    require_binaries!("oc-rsync", LSH_STUB_BIN);
    let tmp = create_tempdir();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    let dirs = fixture(&src);
    let stub = LshRunnerStub::locate().expect("lsh-stub located");

    let out = OcRsyncCliRunner::new()
        .arg("-rt")
        .arg(format!("--rsh={}", stub.path().display()))
        .arg(format!(
            "--rsync-path={}",
            test_support::oc_rsync_bin().display()
        ))
        .arg(format!("localhost:{}/", src.display()))
        .arg(format!("{}/", dest.display()))
        .run()
        .expect("transfer run");
    out.assert_exit(0);

    let stale: Vec<_> = dirs
        .iter()
        .filter(|dir| {
            let meta = fs::metadata(dest.join(dir)).expect("dest dir exists");
            FileTime::from_last_modification_time(&meta).unix_seconds() != DIR_MTIME
        })
        .collect();
    assert!(stale.is_empty(), "directories left un-stamped: {stale:?}");
}
