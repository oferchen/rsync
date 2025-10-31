use super::common::*;
use super::*;

#[test]
fn list_only_lists_entries_without_copying() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    fs::create_dir(&source_dir).expect("create src dir");
    let source_file = source_dir.join("file.txt");
    fs::write(&source_file, b"contents").expect("write source file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;

        let link_path = source_dir.join("link.txt");
        symlink("file.txt", &link_path).expect("create symlink");
    }
    let destination_dir = tmp.path().join("dest");
    fs::create_dir(&destination_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_dir.clone().into_os_string(),
        destination_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    assert!(rendered.contains("file.txt"));
    #[cfg(unix)]
    {
        assert!(rendered.contains("link.txt -> file.txt"));
    }
    assert!(!destination_dir.join("file.txt").exists());
}

#[test]
fn list_only_formats_directory_without_trailing_slash() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_dir.clone().into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    let mut directory_line = None;
    for line in rendered.lines() {
        if line.ends_with("src") {
            directory_line = Some(line.to_string());
            break;
        }
    }

    let directory_line = directory_line.expect("directory entry present");
    assert!(directory_line.starts_with('d'));
    assert!(!directory_line.ends_with('/'));
}

#[test]
fn list_only_matches_rsync_format_for_regular_file() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("data.bin");
    fs::write(&file_path, vec![0u8; 1_234]).expect("write source file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set file permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("data.bin"))
        .expect("file entry present");

    let expected_permissions = "-rw-r--r--";
    let expected_size = format_list_size(1_234, HumanReadableMode::Disabled);
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"))
        + Duration::from_nanos(u64::from(timestamp.nanoseconds()));
    let expected_timestamp = format_list_timestamp(Some(system_time));
    let expected = format!("{expected_permissions} {expected_size} {expected_timestamp} data.bin");

    assert_eq!(file_line, expected);

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--human-readable"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let human_line = rendered
        .lines()
        .find(|line| line.ends_with("data.bin"))
        .expect("file entry present");
    let expected_human_size = format_list_size(1_234, HumanReadableMode::Enabled);
    let expected_human =
        format!("{expected_permissions} {expected_human_size} {expected_timestamp} data.bin");
    assert_eq!(human_line, expected_human);
}

#[test]
fn list_only_formats_special_permission_bits_like_rsync() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let sticky_exec = source_dir.join("exec-special");
    let sticky_plain = source_dir.join("plain-special");

    fs::write(&sticky_exec, b"exec").expect("write exec file");
    fs::write(&sticky_plain, b"plain").expect("write plain file");

    fs::set_permissions(&sticky_exec, fs::Permissions::from_mode(0o7777))
        .expect("set permissions with execute bits");
    fs::set_permissions(&sticky_plain, fs::Permissions::from_mode(0o7666))
        .expect("set permissions without execute bits");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&sticky_exec, timestamp, timestamp).expect("set exec times");
    set_file_times(&sticky_plain, timestamp, timestamp).expect("set plain times");

    let mut source_arg = source_dir.clone().into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"))
        + Duration::from_nanos(u64::from(timestamp.nanoseconds()));
    let expected_timestamp = format_list_timestamp(Some(system_time));

    let expected_exec = format!(
        "-rwsrwsrwt {} {expected_timestamp} exec-special",
        format_list_size(4, HumanReadableMode::Disabled)
    );
    let expected_plain = format!(
        "-rwSrwSrwT {} {expected_timestamp} plain-special",
        format_list_size(5, HumanReadableMode::Disabled)
    );

    let mut exec_line = None;
    let mut plain_line = None;
    for line in rendered.lines() {
        if line.ends_with("exec-special") {
            exec_line = Some(line.to_string());
        } else if line.ends_with("plain-special") {
            plain_line = Some(line.to_string());
        }
    }

    assert_eq!(exec_line.expect("exec entry"), expected_exec);
    assert_eq!(plain_line.expect("plain entry"), expected_plain);
}
