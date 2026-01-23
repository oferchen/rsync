// These tests use Unix-style absolute paths (starting with /)
#[cfg(unix)]
#[test]
fn relative_root_drops_absolute_prefix_without_marker() {
    let operand = OsString::from("/var/log/messages");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    let expected = Path::new("var").join("log").join("messages");
    assert_eq!(spec.relative_root(), Some(expected));
}

#[cfg(unix)]
#[test]
fn relative_root_respects_marker_boundary() {
    let operand = OsString::from("/srv/./data/file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("data/file.txt").to_path_buf())
    );
}

#[test]
fn relative_root_keeps_relative_paths_without_marker() {
    let operand = OsString::from("nested/dir/file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("nested/dir/file.txt").to_path_buf())
    );
}

#[test]
fn relative_root_counts_parent_components_before_marker() {
    let operand = OsString::from("dir/.././trimmed/file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("trimmed/file.txt").to_path_buf())
    );
}

#[cfg(windows)]
#[test]
fn relative_root_handles_windows_drive_prefix() {
    let operand = OsString::from(r"C:\\path\\.\\to\\file.txt");
    let spec = SourceSpec::from_operand(&operand).expect("source spec");
    assert_eq!(
        spec.relative_root(),
        Some(Path::new("to/file.txt").to_path_buf())
    );
}

#[cfg(all(unix, feature = "xattr"))]
#[test]
fn local_copy_options_xattrs_round_trip() {
    let options = LocalCopyOptions::default().xattrs(true);
    assert!(options.preserve_xattrs());
    assert!(
        !LocalCopyOptions::default()
            .xattrs(true)
            .xattrs(false)
            .preserve_xattrs()
    );
}
