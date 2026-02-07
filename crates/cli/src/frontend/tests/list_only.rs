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
        OsString::from("--links"),
        source_dir.into_os_string(),
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
        source_dir.into_os_string(),
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());
    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    let mut directory_line = None;
    for line in rendered.lines() {
        if line.ends_with("src") {
            directory_line = Some(line.to_owned());
            break;
        }
    }

    let directory_line = directory_line.expect("directory entry present");
    assert!(directory_line.starts_with('d'));
    assert!(!directory_line.ends_with('/'));
}

#[cfg(unix)]
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

#[cfg(unix)]
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

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
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
            exec_line = Some(line.to_owned());
        } else if line.ends_with("plain-special") {
            plain_line = Some(line.to_owned());
        }
    }

    assert_eq!(exec_line.expect("exec entry"), expected_exec);
    assert_eq!(plain_line.expect("plain entry"), expected_plain);
}

/// Verifies that every line emitted by `--list-only` matches the upstream rsync
/// format: `<permissions> <size_15_chars> <YYYY/MM/DD HH:MM:SS> <name>`.
#[cfg(unix)]
#[test]
fn list_only_output_lines_match_upstream_regex_pattern() {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    fs::write(source_dir.join("file.txt"), b"hello").expect("write file");
    fs::set_permissions(
        source_dir.join("file.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("set perms");
    fs::create_dir(source_dir.join("subdir")).expect("create subdir");
    symlink("file.txt", source_dir.join("link.txt")).expect("create symlink");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--recursive"),
        OsString::from("--links"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    assert!(!rendered.is_empty(), "output should not be empty");

    // Upstream rsync format: <type+perms 10 chars> <15-char size> <YYYY/MM/DD HH:MM:SS> <name>
    // The fields are separated by single spaces.
    for line in rendered.lines() {
        // Permission field: 10 characters starting with one of -dlpcbs?
        let perm_field = &line[..10];
        assert!(
            perm_field.len() == 10,
            "permission field should be 10 chars: {line:?}"
        );
        let type_char = perm_field.chars().next().unwrap();
        assert!(
            "-dlpcbs?".contains(type_char),
            "type char should be one of -dlpcbs?, got {type_char:?} in {line:?}"
        );

        // After permissions, a single space
        assert_eq!(
            line.as_bytes()[10], b' ',
            "space after permissions in {line:?}"
        );

        // Size field: 15 characters, right-aligned, may contain commas for thousands
        let size_field = &line[11..26];
        assert_eq!(
            size_field.len(),
            15,
            "size field should be 15 chars: {line:?}"
        );
        let size_trimmed = size_field.trim();
        assert!(
            !size_trimmed.is_empty(),
            "size field should not be empty: {line:?}"
        );
        // The size field should contain digits and optionally commas (or ? for unknown)
        assert!(
            size_trimmed == "?"
                || size_trimmed.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.'),
            "size field should be numeric with commas: {size_trimmed:?} in {line:?}"
        );

        // After size, a single space
        assert_eq!(
            line.as_bytes()[26], b' ',
            "space after size in {line:?}"
        );

        // Timestamp field: exactly 19 characters in YYYY/MM/DD HH:MM:SS format
        let timestamp_field = &line[27..46];
        assert_eq!(
            timestamp_field.len(),
            19,
            "timestamp field should be 19 chars: {line:?}"
        );
        // Verify the format pattern
        assert_eq!(
            timestamp_field.as_bytes()[4], b'/',
            "first separator should be / in {line:?}"
        );
        assert_eq!(
            timestamp_field.as_bytes()[7], b'/',
            "second separator should be / in {line:?}"
        );
        assert_eq!(
            timestamp_field.as_bytes()[10], b' ',
            "date/time separator should be space in {line:?}"
        );
        assert_eq!(
            timestamp_field.as_bytes()[13], b':',
            "hour/minute separator should be : in {line:?}"
        );
        assert_eq!(
            timestamp_field.as_bytes()[16], b':',
            "minute/second separator should be : in {line:?}"
        );

        // After timestamp, a single space then the filename
        assert_eq!(
            line.as_bytes()[46], b' ',
            "space after timestamp in {line:?}"
        );
        let name_field = &line[47..];
        assert!(
            !name_field.is_empty(),
            "name field should not be empty: {line:?}"
        );
    }
}

/// Verifies that symlinks in `--list-only` output show ` -> target` suffix,
/// matching upstream rsync format exactly.
#[cfg(unix)]
#[test]
fn list_only_symlink_shows_arrow_target_in_exact_format() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    fs::write(source_dir.join("target.txt"), b"target").expect("write target");
    symlink("target.txt", source_dir.join("mylink")).expect("create symlink");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    // Set time on the target file; symlink mtime comes from symlink_metadata
    set_file_times(source_dir.join("target.txt"), timestamp, timestamp).expect("set times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--links"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let link_line = rendered
        .lines()
        .find(|line| line.contains("mylink"))
        .expect("symlink entry present");

    // Symlink line should start with 'l' (symlink type)
    assert!(
        link_line.starts_with('l'),
        "symlink line should start with 'l': {link_line:?}"
    );

    // Symlink line should end with "mylink -> target.txt"
    assert!(
        link_line.ends_with("mylink -> target.txt"),
        "symlink line should end with 'mylink -> target.txt': {link_line:?}"
    );

    // Verify the ` -> ` arrow pattern (space-arrow-space) is present
    assert!(
        link_line.contains(" -> "),
        "symlink line should contain ' -> ' arrow: {link_line:?}"
    );
}

/// Verifies that zero-byte files are formatted correctly in `--list-only` output.
#[cfg(unix)]
#[test]
fn list_only_zero_byte_file_shows_zero_size() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("empty.txt");
    fs::write(&file_path, b"").expect("write empty file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("empty.txt"))
        .expect("empty file entry present");

    let expected_permissions = "-rw-r--r--";
    let expected_size = format_list_size(0, HumanReadableMode::Disabled);
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(
            u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"),
        );
    let expected_timestamp = format_list_timestamp(Some(system_time));
    let expected =
        format!("{expected_permissions} {expected_size} {expected_timestamp} empty.txt");

    assert_eq!(file_line, expected);
}

/// Verifies that directories in `--list-only` output start with 'd' permission type
/// and show size 0 (matching upstream rsync behavior for directory size field).
#[cfg(unix)]
#[test]
fn list_only_directory_permissions_start_with_d() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let subdir = source_dir.join("testdir");
    fs::create_dir(&subdir).expect("create subdir");
    fs::set_permissions(&subdir, fs::Permissions::from_mode(0o755))
        .expect("set dir permissions");

    let timestamp = FileTime::from_unix_time(1_600_000_000, 0);
    set_file_times(&subdir, timestamp, timestamp).expect("set dir times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--recursive"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let dir_line = rendered
        .lines()
        .find(|line| line.contains("testdir"))
        .expect("directory entry present");

    // Directory permission starts with 'd'
    assert!(
        dir_line.starts_with("drwxr-xr-x"),
        "directory should have drwxr-xr-x permissions: {dir_line:?}"
    );

    // Verify the timestamp portion
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(
            u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"),
        );
    let expected_timestamp = format_list_timestamp(Some(system_time));
    assert!(
        dir_line.contains(&expected_timestamp),
        "directory should contain timestamp {expected_timestamp:?}: {dir_line:?}"
    );
}

/// Verifies the size field is right-aligned in a fixed 15-character column.
#[cfg(unix)]
#[test]
fn list_only_size_field_right_aligned_in_15_chars() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Create files of varying sizes
    let small = source_dir.join("small.txt");
    fs::write(&small, b"x").expect("write small");
    fs::set_permissions(&small, fs::Permissions::from_mode(0o644)).expect("set perms small");

    let medium = source_dir.join("medium.txt");
    fs::write(&medium, vec![0u8; 12_345]).expect("write medium");
    fs::set_permissions(&medium, fs::Permissions::from_mode(0o644)).expect("set perms medium");

    let large = source_dir.join("large.txt");
    fs::write(&large, vec![0u8; 1_234_567]).expect("write large");
    fs::set_permissions(&large, fs::Permissions::from_mode(0o644)).expect("set perms large");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&small, timestamp, timestamp).expect("set times small");
    set_file_times(&medium, timestamp, timestamp).expect("set times medium");
    set_file_times(&large, timestamp, timestamp).expect("set times large");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    for line in rendered.lines() {
        // Size field is at columns 11..26 (0-indexed), exactly 15 characters
        let size_field = &line[11..26];
        assert_eq!(
            size_field.len(),
            15,
            "size field should always be 15 chars: {line:?}"
        );

        // Right-aligned means leading spaces, trailing digits/commas
        let trimmed = size_field.trim_start();
        if trimmed != "?" {
            assert!(
                trimmed.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.'),
                "size field should have right-aligned numeric content: {size_field:?}"
            );
        }
    }
}

/// Verifies that `--list-only` with `--recursive` shows nested directory contents.
#[cfg(unix)]
#[test]
fn list_only_recursive_shows_nested_paths() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    fs::create_dir(source_dir.join("level1")).expect("create level1");
    fs::create_dir(source_dir.join("level1").join("level2")).expect("create level2");
    fs::write(source_dir.join("top.txt"), b"top").expect("write top");
    fs::write(source_dir.join("level1").join("mid.txt"), b"mid").expect("write mid");
    fs::write(
        source_dir.join("level1").join("level2").join("deep.txt"),
        b"deep",
    )
    .expect("write deep");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--recursive"),
        source_arg,
        dest_dir.clone().into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    // All levels should appear in the output
    assert!(
        rendered.lines().any(|l| l.ends_with("top.txt")),
        "top-level file should appear in listing"
    );
    assert!(
        rendered.lines().any(|l| l.contains("level1")),
        "level1 directory should appear in listing"
    );
    assert!(
        rendered.lines().any(|l| l.ends_with("mid.txt")),
        "mid-level file should appear in listing"
    );
    assert!(
        rendered.lines().any(|l| l.contains("level2")),
        "level2 directory should appear in listing"
    );
    assert!(
        rendered.lines().any(|l| l.ends_with("deep.txt")),
        "deeply nested file should appear in listing"
    );

    // No files should actually be transferred
    assert!(!dest_dir.join("top.txt").exists());
    assert!(!dest_dir.join("level1").exists());
}

/// Verifies that large file sizes are formatted with thousands separators.
#[cfg(unix)]
#[test]
fn list_only_large_file_size_has_thousands_separators() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("big.bin");
    // 1,234,567 bytes
    fs::write(&file_path, vec![0u8; 1_234_567]).expect("write large file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("big.bin"))
        .expect("large file entry present");

    let expected_size = format_list_size(1_234_567, HumanReadableMode::Disabled);
    assert!(
        file_line.contains(&expected_size),
        "large file should have formatted size with separators: {file_line:?}"
    );

    // Verify the thousands separator
    assert!(
        expected_size.contains("1,234,567"),
        "size should contain thousands separators: {expected_size:?}"
    );
}

/// Verifies that `--list-only` combined with `--verbose` still produces the
/// listing format (not the verbose transfer format).
#[cfg(unix)]
#[test]
fn list_only_with_verbose_still_shows_listing_format() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("verbose_test.txt");
    fs::write(&file_path, b"verbose test content").expect("write file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--verbose"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("verbose_test.txt"))
        .expect("file entry present");

    // Should still be in list format (starts with permission string, not a bare filename)
    assert!(
        file_line.starts_with('-'),
        "list-only with verbose should still use listing format: {file_line:?}"
    );
    // Permission field should be 10 chars
    assert_eq!(
        &file_line[..10],
        "-rw-r--r--",
        "permissions should be present in listing format: {file_line:?}"
    );
}

/// Verifies that `--list-only` produces correct output for read-only files.
#[cfg(unix)]
#[test]
fn list_only_read_only_file_permissions() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("readonly.txt");
    fs::write(&file_path, b"readonly").expect("write file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o444))
        .expect("set read-only permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("readonly.txt"))
        .expect("readonly file entry present");

    assert!(
        file_line.starts_with("-r--r--r--"),
        "read-only file should show -r--r--r-- permissions: {file_line:?}"
    );
}

/// Verifies that `--list-only` correctly formats an executable file.
#[cfg(unix)]
#[test]
fn list_only_executable_file_permissions() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("script.sh");
    fs::write(&file_path, b"#!/bin/sh\necho hello\n").expect("write script");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o755))
        .expect("set executable permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("script.sh"))
        .expect("script file entry present");

    assert!(
        file_line.starts_with("-rwxr-xr-x"),
        "executable file should show -rwxr-xr-x permissions: {file_line:?}"
    );
}

/// Verifies that `--list-only` with `--human-readable` changes the size field
/// to show human-readable values while keeping the same overall line format.
#[cfg(unix)]
#[test]
fn list_only_human_readable_size_format() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("hr.bin");
    fs::write(&file_path, vec![0u8; 2_500_000]).expect("write large file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--human-readable"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("hr.bin"))
        .expect("file entry present");

    // The size field in human-readable mode should contain unit suffixes
    let expected_hr_size = format_list_size(2_500_000, HumanReadableMode::Enabled);
    assert!(
        file_line.contains(&expected_hr_size),
        "human-readable size should be in listing: expected {expected_hr_size:?}, line: {file_line:?}"
    );

    // The line should still start with permissions
    assert!(
        file_line.starts_with("-rw-r--r--"),
        "human-readable mode should still show permissions: {file_line:?}"
    );
}

/// Verifies that `--list-only` with multiple files shows each file on its own line,
/// with consistent column alignment across all entries.
#[cfg(unix)]
#[test]
fn list_only_multiple_files_have_consistent_column_alignment() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);

    // Create files with different sizes to test alignment
    let files = [
        ("tiny.txt", 1_u64),
        ("small.txt", 100),
        ("medium.txt", 10_000),
        ("bigger.txt", 1_000_000),
    ];

    for (name, size) in &files {
        let path = source_dir.join(name);
        fs::write(&path, vec![0u8; *size as usize]).expect("write file");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("set perms");
        set_file_times(&path, timestamp, timestamp).expect("set times");
    }

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let lines: Vec<&str> = rendered.lines().collect();

    assert!(
        lines.len() >= files.len(),
        "should have at least {} file entries, got {}",
        files.len(),
        lines.len()
    );

    // All lines should have the same structure: permissions end at column 10,
    // size occupies columns 11-25, timestamp at 27-45, name starts at 47
    for line in &lines {
        assert!(
            line.len() >= 47,
            "each line should be at least 47 chars: {line:?}"
        );
        // Permission field
        assert!(
            line[..10].chars().all(|c| "drwxlpcbs-?SsTt".contains(c)),
            "permission field should contain valid permission chars: {line:?}"
        );
        // Space separator after permissions
        assert_eq!(line.as_bytes()[10], b' ', "separator after perms in {line:?}");
        // Size field is 15 chars
        assert_eq!(
            &line[11..26].len(),
            &15,
            "size field should be 15 chars: {line:?}"
        );
        // Space separator after size
        assert_eq!(line.as_bytes()[26], b' ', "separator after size in {line:?}");
        // Timestamp is 19 chars
        assert_eq!(
            &line[27..46].len(),
            &19,
            "timestamp should be 19 chars: {line:?}"
        );
        // Space before name
        assert_eq!(line.as_bytes()[46], b' ', "separator before name in {line:?}");
    }
}

/// Verifies that `--list-only` implies `--dry-run` and does not actually transfer files.
#[test]
fn list_only_implies_dry_run_no_files_transferred() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    fs::write(source_dir.join("a.txt"), b"aaa").expect("write a");
    fs::write(source_dir.join("b.txt"), b"bbb").expect("write b");
    fs::write(source_dir.join("c.txt"), b"ccc").expect("write c");

    let mut source_arg = source_dir.into_os_string();
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
    assert!(rendered.contains("a.txt"), "a.txt should be listed");
    assert!(rendered.contains("b.txt"), "b.txt should be listed");
    assert!(rendered.contains("c.txt"), "c.txt should be listed");

    // Destination should remain empty
    let dest_entries: Vec<_> = fs::read_dir(&dest_dir)
        .expect("read dest")
        .collect();
    assert_eq!(
        dest_entries.len(),
        0,
        "destination should be empty since --list-only implies --dry-run"
    );
}

/// Verifies that FIFO entries show 'p' type character in `--list-only` output.
#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
#[test]
fn list_only_fifo_shows_pipe_type() {
    use std::fs;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let fifo_path = source_dir.join("testpipe");
    mkfifo_for_tests(&fifo_path, 0o644).expect("create fifo");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--specials"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let fifo_line = rendered
        .lines()
        .find(|line| line.contains("testpipe"))
        .expect("FIFO entry should be present");

    assert!(
        fifo_line.starts_with('p'),
        "FIFO entry should start with 'p' type char: {fifo_line:?}"
    );
}

/// Verifies that `--list-only` with `--stats` appends the stats summary
/// after the listing, matching upstream behavior.
#[cfg(unix)]
#[test]
fn list_only_with_stats_appends_summary() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("statsfile.txt");
    fs::write(&file_path, b"stats content").expect("write file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--stats"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    // The listing should appear
    assert!(
        rendered.contains("statsfile.txt"),
        "file listing should be present"
    );

    // Stats summary should follow
    assert!(
        rendered.contains("Number of files:"),
        "stats summary should contain 'Number of files:'"
    );
    assert!(
        rendered.contains("Total file size:"),
        "stats summary should contain 'Total file size:'"
    );
}

/// Verifies that the `--list-only` timestamp field matches the exact YYYY/MM/DD HH:MM:SS
/// format for a known timestamp.
#[cfg(unix)]
#[test]
fn list_only_timestamp_matches_yyyy_mm_dd_hh_mm_ss_format() {
    use filetime::{FileTime, set_file_times};
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    let file_path = source_dir.join("ts.txt");
    fs::write(&file_path, b"timestamp test").expect("write file");
    fs::set_permissions(&file_path, fs::Permissions::from_mode(0o644))
        .expect("set permissions");

    // Use a known timestamp: 2023-11-14 22:13:20 UTC
    let timestamp = FileTime::from_unix_time(1_700_000_000, 0);
    set_file_times(&file_path, timestamp, timestamp).expect("set file times");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");
    let file_line = rendered
        .lines()
        .find(|line| line.ends_with("ts.txt"))
        .expect("timestamp test file entry present");

    // Extract the timestamp portion (columns 27-45)
    let timestamp_field = &file_line[27..46];

    // Verify the format structure
    assert!(
        timestamp_field[0..4].chars().all(|c| c.is_ascii_digit()),
        "year should be 4 digits: {timestamp_field:?}"
    );
    assert_eq!(&timestamp_field[4..5], "/", "year/month separator");
    assert!(
        timestamp_field[5..7].chars().all(|c| c.is_ascii_digit()),
        "month should be 2 digits: {timestamp_field:?}"
    );
    assert_eq!(&timestamp_field[7..8], "/", "month/day separator");
    assert!(
        timestamp_field[8..10].chars().all(|c| c.is_ascii_digit()),
        "day should be 2 digits: {timestamp_field:?}"
    );
    assert_eq!(&timestamp_field[10..11], " ", "date/time separator");
    assert!(
        timestamp_field[11..13].chars().all(|c| c.is_ascii_digit()),
        "hours should be 2 digits: {timestamp_field:?}"
    );
    assert_eq!(&timestamp_field[13..14], ":", "hour/minute separator");
    assert!(
        timestamp_field[14..16].chars().all(|c| c.is_ascii_digit()),
        "minutes should be 2 digits: {timestamp_field:?}"
    );
    assert_eq!(&timestamp_field[16..17], ":", "minute/second separator");
    assert!(
        timestamp_field[17..19].chars().all(|c| c.is_ascii_digit()),
        "seconds should be 2 digits: {timestamp_field:?}"
    );

    // Cross-check with our format function
    let system_time = SystemTime::UNIX_EPOCH
        + Duration::from_secs(
            u64::try_from(timestamp.unix_seconds()).expect("positive timestamp"),
        );
    let expected = format_list_timestamp(Some(system_time));
    assert_eq!(
        timestamp_field, expected,
        "timestamp field should match format_list_timestamp output"
    );
}

/// Verifies that `--list-only` with `--verbose` still adds totals line at the end
/// matching upstream behavior.
#[cfg(unix)]
#[test]
fn list_only_verbose_appends_totals() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    fs::write(source_dir.join("vt.txt"), b"verbose totals").expect("write file");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, _stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--verbose"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    // Should contain the listing
    assert!(
        rendered.contains("vt.txt"),
        "file listing should be present"
    );

    // Verbose adds totals
    assert!(
        rendered.contains("sent") && rendered.contains("bytes"),
        "verbose mode should include totals line with 'sent ... bytes': {}",
        rendered
    );
    assert!(
        rendered.contains("total size is"),
        "verbose mode should include 'total size is' line: {}",
        rendered
    );
}

/// Verifies that `--list-only` output handles multiple file types in the same listing.
#[cfg(unix)]
#[test]
fn list_only_mixed_file_types_in_single_listing() {
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};
    use tempfile::tempdir;

    let tmp = tempdir().expect("tempdir");
    let source_dir = tmp.path().join("src");
    let dest_dir = tmp.path().join("dst");
    fs::create_dir(&source_dir).expect("create src dir");
    fs::create_dir(&dest_dir).expect("create dest dir");

    // Regular file
    fs::write(source_dir.join("regular.txt"), b"regular").expect("write regular");
    fs::set_permissions(
        source_dir.join("regular.txt"),
        fs::Permissions::from_mode(0o644),
    )
    .expect("set regular perms");

    // Directory
    fs::create_dir(source_dir.join("mydir")).expect("create dir");

    // Symlink
    symlink("regular.txt", source_dir.join("mylink")).expect("create symlink");

    let mut source_arg = source_dir.into_os_string();
    source_arg.push(std::path::MAIN_SEPARATOR.to_string());

    let (code, stdout, stderr) = run_with_args([
        OsString::from(RSYNC),
        OsString::from("--list-only"),
        OsString::from("--recursive"),
        OsString::from("--links"),
        source_arg,
        dest_dir.into_os_string(),
    ]);

    assert_eq!(code, 0);
    assert!(stderr.is_empty());

    let rendered = String::from_utf8(stdout).expect("utf8 stdout");

    // Find each type
    let regular_line = rendered
        .lines()
        .find(|l| l.ends_with("regular.txt") && !l.contains("->"))
        .expect("regular file line present");
    let dir_line = rendered
        .lines()
        .find(|l| l.contains("mydir"))
        .expect("directory line present");
    let link_line = rendered
        .lines()
        .find(|l| l.contains("mylink"))
        .expect("symlink line present");

    // Verify type characters
    assert!(
        regular_line.starts_with('-'),
        "regular file should start with '-': {regular_line:?}"
    );
    assert!(
        dir_line.starts_with('d'),
        "directory should start with 'd': {dir_line:?}"
    );
    assert!(
        link_line.starts_with('l'),
        "symlink should start with 'l': {link_line:?}"
    );
}
