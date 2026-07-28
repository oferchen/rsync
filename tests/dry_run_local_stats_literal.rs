//! A local `--dry-run` must report `Literal data: 0` and `Matched data: 0`.
//!
//! upstream: the sender counts the file into `stats.xferred_files` and
//! `stats.total_transferred_size` (`sender.c:342-343`) before the
//! `if (!do_xfers)` guard, but that guard `continue`s before `match_sums()`,
//! the sole accumulator of `stats.literal_data` (`match.c:436`) and
//! `stats.matched_data` (`match.c:121`). So a dry run that WOULD transfer a
//! file still reports `Literal data: 0 bytes` / `Matched data: 0 bytes`, while
//! `Total transferred file size` and the transferred-file count stay non-zero.
//! A real transfer of the same file must still report non-zero literal data.

use std::path::PathBuf;
use std::process::Command;

fn oc_rsync_binary() -> PathBuf {
    let built = PathBuf::from(env!("CARGO_BIN_EXE_oc-rsync"));
    if built.is_file() {
        return built;
    }
    PathBuf::from("oc-rsync")
}

/// Returns the value part of a `--stats` line, e.g. `42 bytes`.
fn stat_line<'a>(stdout: &'a str, prefix: &str) -> &'a str {
    stdout
        .lines()
        .find_map(|line| line.strip_prefix(prefix))
        .map(str::trim)
        .unwrap_or_else(|| panic!("missing {prefix:?} in --stats output:\n{stdout}"))
}

fn run_stats(
    binary: &PathBuf,
    dry_run: bool,
    src: &std::path::Path,
    dest: &std::path::Path,
) -> String {
    let mut cmd = Command::new(binary);
    if dry_run {
        cmd.arg("-n");
    }
    cmd.arg("--stats").arg(src).arg(dest);
    let output = cmd.output().expect("run oc-rsync");
    assert!(
        output.status.success(),
        "transfer must exit 0, got {:?}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn local_dry_run_reports_zero_literal_and_matched() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("f.txt");
    let dest = temp.path().join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    // 42 bytes of content that WOULD be sent as literal data on a real run.
    std::fs::write(&src, b"hello world, some literal content!\n123456\n").expect("write src");

    let stdout = run_stats(&binary, true, &src, &dest);

    // The would-be transfer must never inflate the transferred-byte counters.
    assert_eq!(
        stat_line(&stdout, "Literal data:"),
        "0 bytes",
        "dry run must report zero literal data\nfull stdout:\n{stdout}"
    );
    assert_eq!(
        stat_line(&stdout, "Matched data:"),
        "0 bytes",
        "dry run must report zero matched data\nfull stdout:\n{stdout}"
    );
    // But the scan-derived counters still reflect the file, exactly as upstream.
    assert_eq!(
        stat_line(&stdout, "Total transferred file size:"),
        "42 bytes",
        "dry run still counts the transferred file size\nfull stdout:\n{stdout}"
    );
    assert_eq!(
        stat_line(&stdout, "Number of regular files transferred:"),
        "1",
        "dry run still counts the transferred file\nfull stdout:\n{stdout}"
    );
    // A dry run must not touch the destination.
    assert!(
        !dest.join("f.txt").exists(),
        "dry run must not create the destination file"
    );
}

#[test]
fn local_real_transfer_reports_nonzero_literal() {
    let binary = oc_rsync_binary();
    if !binary.is_file() {
        eprintln!("skip: oc-rsync binary not built at {}", binary.display());
        return;
    }
    let temp = tempfile::tempdir().expect("tempdir");
    let src = temp.path().join("f.txt");
    let dest = temp.path().join("dest");
    std::fs::create_dir(&dest).expect("mkdir dest");
    std::fs::write(&src, b"hello world, some literal content!\n123456\n").expect("write src");

    let stdout = run_stats(&binary, false, &src, &dest);

    // A real transfer of a new file sends the whole file as literal data.
    assert_eq!(
        stat_line(&stdout, "Literal data:"),
        "42 bytes",
        "real transfer must report the full literal data\nfull stdout:\n{stdout}"
    );
    assert!(
        dest.join("f.txt").exists(),
        "real transfer must create the destination file"
    );
}
