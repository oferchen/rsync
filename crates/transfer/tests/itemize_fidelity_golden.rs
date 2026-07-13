//! Golden itemize-fidelity regression test for local `-a -ii` transfers.
//!
//! Every expected itemize row in this file is byte-for-byte what upstream
//! rsync 3.4.4 (protocol 32) prints for the same scenario. Pinning them here
//! lets CI catch an itemize-output regression WITHOUT needing an upstream
//! binary on the runner: if oc-rsync's `%i` rendering drifts from upstream in
//! any position - the update glyph, the file-type glyph, or any of the nine
//! attribute flags (checksum/size/time/perms/owner/group/...) - one of these
//! assertions fails.
//!
//! The scenarios are driven off explicit timestamps (`T1`/`T2`) so the output
//! is deterministic. In particular `symlink_retarget` reproduces the case
//! upstream renders `cLc........` (a retargeted symlink whose mtime falls in
//! the same whole second as the old link's, so no `t` glyph): upstream
//! `itemize()` (generator.c:526-527) tests the time via `mtime_differs()` ->
//! `same_time()` (util1.c:1478), which with the default `modify_window == 0`
//! compares whole seconds only and ignores the fractional part. An exact
//! nanosecond comparison would spuriously light the `t` glyph (`cLc.t......`);
//! this test guards that boundary.
//!
//! # Upstream Reference
//!
//! - `log.c:695-746` - `%i` itemize string construction.
//! - `generator.c:508-549` - `itemize()` derives the attribute flags.
//! - `util1.c:1478` - `same_time()` whole-second comparison.

#![cfg(unix)]

use std::env;
use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::Command;

use filetime::{FileTime, set_file_times, set_symlink_file_times};

/// 2020-01-01 00:00:00 UTC.
const T1: i64 = 1_577_836_800;
/// 2023-06-06 12:00:00 UTC.
const T2: i64 = 1_686_052_800;

/// Locates the workspace `oc-rsync` binary the test runner built.
///
/// Prefers Cargo's `CARGO_BIN_EXE_oc-rsync`, otherwise walks up from the test
/// executable until a `target/` directory is found (mirrors the lookup in the
/// sibling integration tests).
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

/// Whole-second `FileTime` for a POSIX epoch second.
fn ft(secs: i64) -> FileTime {
    FileTime::from_unix_time(secs, 0)
}

/// Writes `content` to `path`.
fn write(path: &Path, content: &[u8]) {
    fs::write(path, content).expect("write file");
}

/// Sets the mtime (and atime) of a regular file or directory.
fn set_mtime(path: &Path, time: FileTime) {
    set_file_times(path, time, time).expect("set file times");
}

/// Sets the mtime (and atime) of a symlink itself (no target follow).
fn set_link_mtime(path: &Path, time: FileTime) {
    set_symlink_file_times(path, time, time).expect("set symlink times");
}

/// A single itemize scenario: build `base/src` and `base/dst`, run
/// `oc-rsync -a -ii [extra_args] src/ dst/`, and assert the filtered itemize
/// rows equal `expected` in order.
struct Scenario {
    name: &'static str,
    extra_args: &'static [&'static str],
    setup: fn(&Path),
    expected: &'static [&'static str],
}

/// Keeps only itemize rows: a line whose first two bytes are an
/// update-glyph + file-type-glyph, or a `*deleting` line. Drops the
/// "sending incremental file list" preamble and the byte-count summary.
///
/// upstream: log.c:695-746 - `%i` produces `YXcstpoguax`, where `Y` is one of
/// `<>ch.*` and `X` is one of `fdDLS`; deletions render `*deleting`.
fn is_itemize_row(line: &str) -> bool {
    if line.starts_with("*deleting") {
        return true;
    }
    let bytes = line.as_bytes();
    bytes.len() >= 2
        && matches!(bytes[0], b'<' | b'>' | b'c' | b'h' | b'.')
        && matches!(bytes[1], b'f' | b'd' | b'D' | b'L' | b'S')
}

fn run_scenario(bin: &Path, scenario: &Scenario) {
    let temp = tempfile::tempdir().expect("tempdir");
    let base = temp.path();
    fs::create_dir_all(base.join("src")).expect("mkdir src");
    fs::create_dir_all(base.join("dst")).expect("mkdir dst");
    (scenario.setup)(base);

    let src = format!("{}/", base.join("src").display());
    let dst = format!("{}/", base.join("dst").display());
    let output = Command::new(bin)
        .arg("-a")
        .arg("-ii")
        .args(scenario.extra_args)
        .arg(&src)
        .arg(&dst)
        .output()
        .expect("run oc-rsync");

    assert!(
        output.status.success(),
        "scenario {}: oc-rsync exited with {:?}\nstderr:\n{}",
        scenario.name,
        output.status.code(),
        String::from_utf8_lossy(&output.stderr),
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let rows: Vec<&str> = stdout.lines().filter(|l| is_itemize_row(l)).collect();

    assert_eq!(
        rows, scenario.expected,
        "scenario {}: itemize rows diverged from upstream 3.4.4 golden\n\
         full stdout:\n{}",
        scenario.name, stdout,
    );
}

/// new_file: a freshly transferred file into a pre-existing destination
/// directory whose mtime differs from the source root.
fn setup_new_file(base: &Path) {
    let src = base.join("src");
    write(&src.join("f1"), b"hi");
    set_mtime(&src.join("f1"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&base.join("dst"), ft(T2));
}

/// changed_content: existing file with different content and mtime; the
/// destination root already matches the source root mtime.
fn setup_changed_content(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    write(&src.join("f1"), b"helloworld");
    write(&dst.join("f1"), b"old");
    set_mtime(&src.join("f1"), ft(T2));
    set_mtime(&dst.join("f1"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T1));
}

/// time_only: identical content, differing mtime.
fn setup_time_only(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    write(&src.join("f1"), b"abcd");
    write(&dst.join("f1"), b"abcd");
    set_mtime(&src.join("f1"), ft(T2));
    set_mtime(&dst.join("f1"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T1));
}

/// unchanged: identical content and mtime - only shown because of `-ii`.
fn setup_unchanged(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    write(&src.join("f1"), b"abcd");
    write(&dst.join("f1"), b"abcd");
    set_mtime(&src.join("f1"), ft(T1));
    set_mtime(&dst.join("f1"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T1));
}

/// perms: identical content and mtime, differing permission bits.
fn setup_perms(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    write(&src.join("f1"), b"abcd");
    write(&dst.join("f1"), b"abcd");
    fs::set_permissions(src.join("f1"), fs::Permissions::from_mode(0o755)).expect("chmod src");
    fs::set_permissions(dst.join("f1"), fs::Permissions::from_mode(0o644)).expect("chmod dst");
    set_mtime(&src.join("f1"), ft(T1));
    set_mtime(&dst.join("f1"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T1));
}

/// new_dir: a whole subtree transferred into a pre-existing destination root.
fn setup_new_dir(base: &Path) {
    let src = base.join("src");
    fs::create_dir_all(src.join("sub")).expect("mkdir sub");
    write(&src.join("sub").join("c"), b"c");
    set_mtime(&src.join("sub").join("c"), ft(T1));
    set_mtime(&src.join("sub"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&base.join("dst"), ft(T2));
}

/// dir_time: an existing subdirectory whose mtime differs from the source.
fn setup_dir_time(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    fs::create_dir_all(src.join("sub")).expect("mkdir src/sub");
    fs::create_dir_all(dst.join("sub")).expect("mkdir dst/sub");
    write(&src.join("sub").join("c"), b"c");
    write(&dst.join("sub").join("c"), b"c");
    set_mtime(&src.join("sub").join("c"), ft(T1));
    set_mtime(&dst.join("sub").join("c"), ft(T1));
    set_mtime(&src.join("sub"), ft(T2));
    set_mtime(&dst.join("sub"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T1));
}

/// new_symlink: a symlink created into a pre-existing destination root.
fn setup_new_symlink(base: &Path) {
    let src = base.join("src");
    symlink("target", src.join("l1")).expect("create symlink");
    set_link_mtime(&src.join("l1"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&base.join("dst"), ft(T2));
}

/// symlink_retarget: the destination symlink already exists but points
/// elsewhere; the two link mtimes share a whole second (differing only in
/// nanoseconds), so upstream reports `cLc........` - NO `t` glyph. This is the
/// boundary the whole-second `same_time()` comparison guards.
fn setup_symlink_retarget(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    symlink("newtgt", src.join("l1")).expect("create src symlink");
    symlink("oldtgt", dst.join("l1")).expect("create dst symlink");
    // Same whole second, different nanoseconds: must NOT light `t` upstream.
    set_link_mtime(&src.join("l1"), FileTime::from_unix_time(T1, 500_000_000));
    set_link_mtime(&dst.join("l1"), FileTime::from_unix_time(T1, 100_000_000));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T1));
}

/// new_hardlink: two names for one inode transferred with `-H` into a
/// pre-existing destination root.
fn setup_new_hardlink(base: &Path) {
    let src = base.join("src");
    write(&src.join("a"), b"data");
    fs::hard_link(src.join("a"), src.join("b")).expect("hard link");
    set_mtime(&src.join("a"), ft(T1));
    set_mtime(&src.join("b"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&base.join("dst"), ft(T2));
}

/// delete: a stale file in the destination is removed with `--delete`.
fn setup_delete(base: &Path) {
    let (src, dst) = (base.join("src"), base.join("dst"));
    write(&src.join("f1"), b"f");
    write(&dst.join("extra"), b"x");
    set_mtime(&src.join("f1"), ft(T1));
    set_mtime(&dst.join("extra"), ft(T1));
    set_mtime(&src, ft(T1));
    set_mtime(&dst, ft(T2));
}

const SCENARIOS: &[Scenario] = &[
    Scenario {
        name: "new_file",
        extra_args: &[],
        setup: setup_new_file,
        expected: &[".d..t...... ./", ">f+++++++++ f1"],
    },
    Scenario {
        name: "changed_content",
        extra_args: &[],
        setup: setup_changed_content,
        expected: &[".d          ./", ">f.st...... f1"],
    },
    Scenario {
        name: "time_only",
        extra_args: &[],
        setup: setup_time_only,
        expected: &[".d          ./", ">f..t...... f1"],
    },
    Scenario {
        name: "unchanged",
        extra_args: &[],
        setup: setup_unchanged,
        expected: &[".d          ./", ".f          f1"],
    },
    Scenario {
        name: "perms",
        extra_args: &[],
        setup: setup_perms,
        expected: &[".d          ./", ".f...p..... f1"],
    },
    Scenario {
        name: "new_dir",
        extra_args: &[],
        setup: setup_new_dir,
        expected: &[".d..t...... ./", "cd+++++++++ sub/", ">f+++++++++ sub/c"],
    },
    Scenario {
        name: "dir_time",
        extra_args: &[],
        setup: setup_dir_time,
        expected: &[".d          ./", ".d..t...... sub/", ".f          sub/c"],
    },
    Scenario {
        name: "new_symlink",
        extra_args: &[],
        setup: setup_new_symlink,
        expected: &[".d..t...... ./", "cL+++++++++ l1 -> target"],
    },
    Scenario {
        name: "symlink_retarget",
        extra_args: &[],
        setup: setup_symlink_retarget,
        expected: &[".d          ./", "cLc........ l1 -> newtgt"],
    },
    Scenario {
        name: "new_hardlink",
        extra_args: &["-H"],
        setup: setup_new_hardlink,
        expected: &[".d..t...... ./", ">f+++++++++ b", "hf+++++++++ a => b"],
    },
    Scenario {
        name: "delete",
        extra_args: &["--delete"],
        setup: setup_delete,
        expected: &["*deleting   extra", ".d..t...... ./", ">f+++++++++ f1"],
    },
];

#[test]
fn itemize_rows_match_upstream_3_4_4_golden() {
    let Some(bin) = locate_oc_rsync() else {
        eprintln!("skipping: oc-rsync binary not found in target/");
        return;
    };
    for scenario in SCENARIOS {
        run_scenario(&bin, scenario);
    }
}
