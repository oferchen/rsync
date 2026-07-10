//! Regression test for the deprecated `--old-dirs` / `--old-d` flags.
//!
//! upstream: options.c:631-632 declare `--old-dirs`/`--old-d` as
//! `POPT_ARG_VAL` setting `xfer_dirs = 4`, and options.c:2197-2199 resolves
//! that after the argv scan:
//!
//! ```c
//! if (xfer_dirs >= 4) {
//!     parse_filter_str(&filter_list, "- /*/*", rule_template(0), 0);
//!     recurse = xfer_dirs = 1;
//! }
//! ```
//!
//! So the flags force recursion on AND append `- /*/*` to the tail of the
//! filter list (exclude.c:parse_filter_str appends each rule at the end). The
//! anchored `/*/*` rule excludes every path with two or more components, so
//! only the top-level entries survive: top-level files are transferred and
//! top-level directories are created but come across empty. Nothing at depth
//! two or deeper is transferred. This has nothing to do with `--mkpath`.
//!
//! Ground truth captured from upstream `rsync 3.4.4`:
//!
//! ```sh
//! $ rsync --old-dirs src/ dst/   # src: f0, a/f1, a/b/f2
//! $ find dst
//! dst
//! dst/a       # empty directory
//! dst/f0
//! ```

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Locates the workspace `oc-rsync` binary the test runner built.
fn locate_oc_rsync() -> Option<PathBuf> {
    if let Some(p) = env::var_os("CARGO_BIN_EXE_oc-rsync") {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    let exe = env::current_exe().ok()?;
    let mut dir = exe.parent()?;
    let name = format!("oc-rsync{}", env::consts::EXE_SUFFIX);
    while !dir.ends_with("target") {
        let candidate = dir.join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = dir.parent()?;
    }
    for sub in ["debug", "release"] {
        let candidate = dir.join(sub).join(&name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Builds `src/{f0, a/f1, a/b/f2}` under `root` and returns the source dir.
fn build_three_level_tree(root: &std::path::Path) -> PathBuf {
    let src = root.join("src");
    fs::create_dir_all(src.join("a/b")).expect("mkdir src/a/b");
    fs::write(src.join("f0"), b"f0\n").expect("write f0");
    fs::write(src.join("a/f1"), b"f1\n").expect("write a/f1");
    fs::write(src.join("a/b/f2"), b"f2\n").expect("write a/b/f2");
    src
}

/// `--old-dirs` recurses (creating top-level directories) but the appended
/// `- /*/*` rule excludes everything two or more levels deep, so depth-1 and
/// depth-2 files never transfer while the top-level file does.
#[test]
fn old_dirs_transfers_only_top_level() {
    let Some(rsync_bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = build_three_level_tree(tmp.path());
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&dst).expect("mkdir dst");

    let src_arg = {
        let mut s = src.clone().into_os_string();
        s.push("/");
        s
    };

    let status = Command::new(&rsync_bin)
        .arg("--old-dirs")
        .arg(&src_arg)
        .arg(&dst)
        .status()
        .expect("spawn oc-rsync");
    assert!(status.success(), "oc-rsync exited with {status:?}");

    // Depth 0: the top-level file is transferred.
    assert!(dst.join("f0").is_file(), "top-level f0 should transfer");
    // The top-level directory is created (recursion is on) ...
    assert!(dst.join("a").is_dir(), "top-level dir a/ should be created");
    // ... but its contents are excluded by `- /*/*`.
    assert!(
        !dst.join("a/f1").exists(),
        "depth-1 file a/f1 must be excluded by `- /*/*`"
    );
    assert!(
        !dst.join("a/b").exists(),
        "depth-1 dir a/b must be excluded by `- /*/*`"
    );
    assert!(
        !dst.join("a/b/f2").exists(),
        "depth-2 file a/b/f2 must be excluded by `- /*/*`"
    );

    // `--old-dir` (single form via --old-d) must behave identically.
    let dst_short = tmp.path().join("dst_short");
    fs::create_dir_all(&dst_short).expect("mkdir dst_short");
    let status = Command::new(&rsync_bin)
        .arg("--old-d")
        .arg(&src_arg)
        .arg(&dst_short)
        .status()
        .expect("spawn oc-rsync");
    assert!(status.success(), "oc-rsync --old-d exited with {status:?}");
    assert!(dst_short.join("f0").is_file());
    assert!(dst_short.join("a").is_dir());
    assert!(!dst_short.join("a/f1").exists());
    assert!(!dst_short.join("a/b/f2").exists());
}

/// A plain `--no-mkpath` run must remain a normal recursive-capable copy and
/// must NOT gain the `- /*/*` depth limit that `--old-dirs` injects. This
/// pins the decoupling of `--old-dirs` from `--no-mkpath`.
#[test]
fn no_mkpath_still_copies_full_tree() {
    let Some(rsync_bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };

    let tmp = tempfile::tempdir().expect("create tempdir");
    let src = build_three_level_tree(tmp.path());
    let dst = tmp.path().join("dst");
    fs::create_dir_all(&dst).expect("mkdir dst");

    let src_arg = {
        let mut s = src.clone().into_os_string();
        s.push("/");
        s
    };

    // `-r --no-mkpath`: recursion requested explicitly; --no-mkpath only
    // suppresses destination path-component creation and must not filter.
    let status = Command::new(&rsync_bin)
        .arg("-r")
        .arg("--no-mkpath")
        .arg(&src_arg)
        .arg(&dst)
        .status()
        .expect("spawn oc-rsync");
    assert!(status.success(), "oc-rsync exited with {status:?}");

    assert!(dst.join("f0").is_file(), "f0 should transfer");
    assert!(
        dst.join("a/f1").is_file(),
        "--no-mkpath must not exclude depth-1 a/f1"
    );
    assert!(
        dst.join("a/b/f2").is_file(),
        "--no-mkpath must not exclude depth-2 a/b/f2"
    );
}
