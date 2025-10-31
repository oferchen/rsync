use super::common::*;
use super::*;

#[cfg(unix)]
#[test]
fn info_progress2_enables_progress_output() {
    use std::os::unix::fs::FileTypeExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-fifo.in");
    mkfifo_for_tests(&source, 0o600).expect("mkfifo");

    let destination = tmp.path().join("info-fifo.out");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=progress2"),
        OsString::from("--specials"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("progress output is UTF-8");
    assert!(!rendered.contains("info-fifo.in"));
    assert!(rendered.contains("to-chk=0/1"));
    assert!(rendered.contains("0.00kB/s"));

    let metadata = std::fs::symlink_metadata(&destination).expect("stat destination");
    assert!(metadata.file_type().is_fifo());
}

#[test]
fn info_stats_enables_summary_block() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-stats.txt");
    let destination = tmp.path().join("info-stats.out");
    let payload = b"statistics";
    std::fs::write(&source, payload).expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=stats"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("stats output is UTF-8");
    let expected_size = payload.len();
    assert!(rendered.contains("Number of files: 1 (reg: 1)"));
    assert!(rendered.contains(&format!("Total file size: {expected_size} bytes")));
    assert!(rendered.contains("Literal data:"));
    assert!(rendered.contains("\n\nsent"));
    assert!(rendered.contains("total size is"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        payload
    );
}

#[test]
fn info_none_disables_progress_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("info-none.txt");
    let destination = tmp.path().join("info-none.out");
    std::fs::write(&source, b"payload").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--progress"),
        OsString::from("--info=none"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(!rendered.contains("to-chk"));
    assert!(rendered.trim().is_empty());
}

#[test]
fn info_help_lists_supported_flags() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, INFO_HELP_TEXT.as_bytes());
}

#[test]
fn debug_help_lists_supported_flags() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--debug=help")]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert_eq!(stdout, DEBUG_HELP_TEXT.as_bytes());
}

#[test]
fn info_rejects_unknown_flag() {
    let (code, stdout, stderr) = run_with_args([OsStr::new(RSYNC), OsStr::new("--info=unknown")]);

    assert_eq!(code, 1);
    assert!(stdout.is_empty());
    let rendered = String::from_utf8(stderr).expect("stderr utf8");
    assert!(rendered.contains("invalid --info flag"));
}

#[test]
fn info_accepts_comma_separated_tokens() {
    let flags = vec![OsString::from("progress,name2,stats")];
    let settings = parse_info_flags(&flags).expect("flags parse");
    assert!(matches!(settings.progress, ProgressSetting::PerFile));
    assert_eq!(settings.name, Some(NameOutputLevel::UpdatedAndUnchanged));
    assert_eq!(settings.stats, Some(true));
}

#[test]
fn info_rejects_empty_segments() {
    let flags = vec![OsString::from("progress,,stats")];
    let error = parse_info_flags(&flags).err().expect("parse should fail");
    assert!(error.to_string().contains("--info flag must not be empty"));
}

#[test]
fn debug_accepts_comma_separated_tokens() {
    let flags = vec![OsString::from("checksum,io")];
    let settings = parse_debug_flags(&flags).expect("flags parse");
    assert!(!settings.help_requested);
    assert_eq!(
        settings.flags,
        vec![OsString::from("checksum"), OsString::from("io")]
    );
}

#[test]
fn debug_rejects_empty_segments() {
    let flags = vec![OsString::from("checksum,,io")];
    let error = parse_debug_flags(&flags).err().expect("parse should fail");
    assert!(error.to_string().contains("--debug flag must not be empty"));
}

#[test]
fn info_name_emits_filenames_without_verbose() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("name.txt");
    let destination = tmp.path().join("name.out");
    std::fs::write(&source, b"name-info").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered.contains("name.txt"));
    assert!(rendered.contains("sent"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"name-info"
    );
}

#[test]
fn info_name0_suppresses_verbose_output() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("quiet.txt");
    let destination = tmp.path().join("quiet.out");
    std::fs::write(&source, b"quiet").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        OsString::from("--info=name0"),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(!rendered.contains("quiet.txt"));
    assert!(rendered.contains("sent"));
}

#[test]
fn info_name2_reports_unchanged_entries() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("unchanged.txt");
    let destination = tmp.path().join("unchanged.out");
    std::fs::write(&source, b"unchanged").expect("write source");

    let initial = run_with_args([
        OsString::from(RSYNC),
        source.clone().into_os_string(),
        destination.clone().into_os_string(),
    ]);
    assert_eq!(initial.0, 0);
    assert!(initial.1.is_empty());
    assert!(initial.2.is_empty());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--info=name2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("stdout utf8");
    assert!(rendered.contains("unchanged.txt"));
}
