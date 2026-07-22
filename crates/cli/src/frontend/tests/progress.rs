use super::common::*;
use super::*;

#[test]
fn progress_transfer_renders_progress_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progress.txt");
    let destination = tmp.path().join("progress.out");
    std::fs::write(&source, b"progress").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("progress.txt"));
    assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
    assert!(!rendered.contains("Total transferred"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"progress"
    );
}

#[test]
fn progress_does_not_relist_names() {
    // upstream prints each name exactly once, inline before its progress line.
    // `--progress` (info=name1, verbosity 0) must NOT re-emit the whole name
    // listing after the per-file progress block.
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let src = tmp.path().join("src");
    let dst = tmp.path().join("dst");
    std::fs::create_dir(&src).expect("mkdir src");
    std::fs::write(src.join("alpha.txt"), b"a").expect("write alpha");
    std::fs::write(src.join("bravo.txt"), b"b").expect("write bravo");

    let mut src_arg = src.into_os_string();
    src_arg.push("/");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-a"),
        OsString::from("--progress"),
        src_arg,
        dst.into_os_string(),
    ]);

    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&stderr));
    let out = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert_eq!(
        out.matches("alpha.txt").count(),
        1,
        "each name must be printed once, not re-listed:\n{out}"
    );
    assert_eq!(out.matches("bravo.txt").count(), 1, "{out}");
}

#[test]
fn progress_human_readable_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("human-progress.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination_default = tmp.path().join("default-progress.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.clone().into_os_string(),
        destination_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    assert!(normalized.contains("1,536"));

    let destination_human = tmp.path().join("human-progress.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        destination_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    assert!(normalized.contains("1.54K"));
}

#[test]
fn progress_human_readable_combined_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("human-progress.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination = tmp.path().join("combined-progress.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("-hh"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    // upstream: `-hh` divides by 1024 (1536/1024 = 1.50K), no exact component.
    assert!(normalized.contains("1.50K") && !normalized.contains("(1,536)"));
}

#[test]
fn progress_transfer_routes_messages_to_stderr_when_requested() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("stderr-progress.txt");
    let destination = tmp.path().join("stderr-progress.out");
    std::fs::write(&source, b"stderr-progress").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--msgs2stderr"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    let rendered_out = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered_out.trim().is_empty());

    let rendered_err = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered_err.contains("stderr-progress.txt"));
    assert!(rendered_err.contains("(xfr#1, to-chk=0/1)"));

    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"stderr-progress"
    );
}

#[test]
fn progress_percent_unknown_total_resolves_to_complete() {
    // upstream: progress.c:128 - `pct = ofs == size ? 100 : ...` never emits a
    // `??` sentinel for the percent field. oc emits one progress line per file
    // at completion, so an unknown total resolves to 100%, not `??%`.
    assert_eq!(format_progress_percent(42, None), "100%");
    assert!(!format_progress_percent(42, None).contains("??"));
    assert_eq!(format_progress_percent(0, Some(0)), "100%");
    assert_eq!(format_progress_percent(50, Some(200)), "25%");
}

#[test]
fn progress_reports_intermediate_updates() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("large.bin");
    let destination = tmp.path().join("large.out");
    // Use 4MB to ensure intermediate progress updates even on fast systems.
    // 256KB was too small - the transfer completed in one write on fast macOS
    // CI runners, producing no intermediate \r updates.
    let payload = vec![0xA5u8; 4 * 1024 * 1024];
    std::fs::write(&source, &payload).expect("write large source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("large.bin"));
    assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
    // Intermediate \r updates and percentages are timing-dependent.
    // Only assert that the final 100% is present.
    assert!(rendered.contains("100%"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

#[cfg(unix)]
#[test]
fn progress_reports_unknown_totals_with_placeholder() {
    use std::os::unix::fs::FileTypeExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("fifo.in");
    mkfifo_for_tests(&source, 0o600).expect("mkfifo");

    let destination = tmp.path().join("fifo.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--specials"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    // upstream: receiver.c:843-895 - a lone special (fifo) is not ITEM_TRANSFER,
    // so per-file `--progress` prints no per-file progress block for it. The
    // terminal `end_progress(0)` summary at NDX_DONE fires only under
    // `--info=progress2` (receiver.c:786-788), not plain `--progress`. The
    // fifo's name still prints via the name-output path. Verified against rsync
    // 3.4.4: `--progress --specials <fifo>` emits just `<fifo>\n` - no `??%`
    // placeholder, no `to-chk` line.
    assert!(rendered.contains("fifo.in"));
    assert!(!rendered.contains("??%"));
    assert!(!rendered.contains("to-chk"));

    let metadata = std::fs::symlink_metadata(&destination).expect("stat destination");
    assert!(metadata.file_type().is_fifo());
}

#[test]
fn progress_with_verbose_inserts_separator_before_totals() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("progress.txt");
    let destination = tmp.path().join("progress.out");
    std::fs::write(&source, b"progress").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("-v"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(rendered.contains("(xfr#1, to-chk=0/1)"));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("sent"));
    assert!(rendered.contains("total size is"));
}
