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
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output utf8");
    let normalized = rendered.replace('\r', "\n");
    assert!(normalized.contains("1.54K (1,536)"));
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
fn progress_percent_placeholder_used_for_unknown_totals() {
    assert_eq!(format_progress_percent(42, None), "??%");
    assert_eq!(format_progress_percent(0, Some(0)), "100%");
    assert_eq!(format_progress_percent(50, Some(200)), "25%");
}

#[test]
fn progress_reports_intermediate_updates() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("large.bin");
    let destination = tmp.path().join("large.out");
    let payload = vec![0xA5u8; 256 * 1024];
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
    assert!(rendered.contains("\r"));
    assert!(rendered.contains(" 50%"));
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
    assert!(rendered.contains("fifo.in"));
    assert!(rendered.contains("??%"));
    assert!(rendered.contains("to-chk=0/1"));

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
