use super::common::*;
use super::*;

#[test]
fn verbose_transfer_emits_event_lines() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("file.txt");
    let destination = tmp.path().join("out.txt");
    std::fs::write(&source, b"verbose").expect("write source");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("file.txt"));
    assert!(!rendered.contains("Total transferred"));
    assert!(rendered.contains("sent 7 bytes  received 7 bytes"));
    assert!(rendered.contains("total size is 7  speedup is 0.50"));
    assert_eq!(
        std::fs::read(destination).expect("read destination"),
        b"verbose"
    );
}

#[cfg(unix)]
#[test]
fn verbose_transfer_reports_skipped_specials() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_fifo = tmp.path().join("skip.pipe");
    mkfifo_for_tests(&source_fifo, 0o600).expect("mkfifo");

    let destination = tmp.path().join("dest.pipe");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-v"),
        source_fifo.into_os_string(),
        destination.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    assert!(std::fs::symlink_metadata(&destination).is_err());

    let rendered = String::from_utf8(stdout).expect("verbose output is UTF-8");
    assert!(rendered.contains("skipping non-regular file \"skip.pipe\""));
}

#[test]
fn verbose_human_readable_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sizes.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let dest_default = tmp.path().join("default.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        source.clone().into_os_string(),
        dest_default.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1,536 bytes"));

    let dest_human = tmp.path().join("human.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--human-readable"),
        source.into_os_string(),
        dest_human.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1.54K bytes"));
}

#[test]
fn verbose_human_readable_combined_formats_sizes() {
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source = tmp.path().join("sizes.bin");
    std::fs::write(&source, vec![0u8; 1_536]).expect("write source");

    let destination = tmp.path().join("combined.bin");
    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-vv"),
        OsString::from("--human-readable=2"),
        source.into_os_string(),
        destination.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("verbose output utf8");
    assert!(rendered.contains("1.54K (1,536) bytes"));
}

#[cfg(unix)]
#[test]
fn verbose_output_includes_symlink_target() {
    use std::fs;
    use std::os::unix::fs::symlink;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"contents").expect("write source file");
    let link_path = source_dir.join("link.txt");
    symlink("file.txt", &link_path).expect("create symlink");

    let destination_dir = tmp.path().join("dest");
    fs::create_dir(&destination_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("-av"),
        source_dir.into_os_string(),
        destination_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    assert!(rendered.contains("link.txt -> file.txt"));
}
