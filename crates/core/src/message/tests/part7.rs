// Comprehensive path normalization tests

#[test]
fn normalize_path_handles_relative_paths() {
    let normalized = normalize_path(Path::new("foo/bar/baz.txt"));
    assert_eq!(normalized, "foo/bar/baz.txt");
}

#[test]
fn normalize_path_handles_single_component() {
    let normalized = normalize_path(Path::new("file.txt"));
    assert_eq!(normalized, "file.txt");
}

#[test]
fn normalize_path_collapses_current_dir_in_middle() {
    let normalized = normalize_path(Path::new("foo/./bar/./baz.txt"));
    assert_eq!(normalized, "foo/bar/baz.txt");
}

#[test]
fn normalize_path_collapses_current_dir_at_start() {
    let normalized = normalize_path(Path::new("./foo/bar"));
    assert_eq!(normalized, "foo/bar");
}

#[test]
fn normalize_path_collapses_current_dir_at_end() {
    let normalized = normalize_path(Path::new("foo/bar/."));
    assert_eq!(normalized, "foo/bar");
}

#[test]
fn normalize_path_handles_only_current_dir() {
    let normalized = normalize_path(Path::new("."));
    assert_eq!(normalized, ".");
}

#[test]
fn normalize_path_resolves_parent_dir_in_middle() {
    let normalized = normalize_path(Path::new("foo/bar/../baz.txt"));
    assert_eq!(normalized, "foo/baz.txt");
}

#[test]
fn normalize_path_resolves_multiple_parent_dirs() {
    let normalized = normalize_path(Path::new("foo/bar/baz/../../qux.txt"));
    assert_eq!(normalized, "foo/qux.txt");
}

#[test]
fn normalize_path_resolves_parent_to_root() {
    let normalized = normalize_path(Path::new("foo/../bar.txt"));
    assert_eq!(normalized, "bar.txt");
}

#[test]
fn normalize_path_preserves_leading_parent_dirs() {
    let normalized = normalize_path(Path::new("../foo/bar.txt"));
    assert_eq!(normalized, "../foo/bar.txt");
}

#[test]
fn normalize_path_preserves_multiple_leading_parent_dirs() {
    let normalized = normalize_path(Path::new("../../foo/bar.txt"));
    assert_eq!(normalized, "../../foo/bar.txt");
}

#[test]
fn normalize_path_resolves_parent_after_leading_parent() {
    let normalized = normalize_path(Path::new("../foo/../bar.txt"));
    assert_eq!(normalized, "../bar.txt");
}

#[test]
fn normalize_path_handles_complex_relative_sequence() {
    let normalized = normalize_path(Path::new("./foo/./bar/../baz/./qux.txt"));
    assert_eq!(normalized, "foo/baz/qux.txt");
}

#[test]
fn normalize_path_handles_absolute_unix_path() {
    let normalized = normalize_path(Path::new("/foo/bar/baz.txt"));
    assert_eq!(normalized, "/foo/bar/baz.txt");
}

#[test]
fn normalize_path_resolves_parent_in_absolute_path() {
    let normalized = normalize_path(Path::new("/foo/bar/../baz.txt"));
    assert_eq!(normalized, "/foo/baz.txt");
}

#[test]
fn normalize_path_collapses_current_in_absolute_path() {
    let normalized = normalize_path(Path::new("/foo/./bar/./baz.txt"));
    assert_eq!(normalized, "/foo/bar/baz.txt");
}

#[test]
fn normalize_path_ignores_parent_beyond_root() {
    let normalized = normalize_path(Path::new("/foo/../../bar.txt"));
    assert_eq!(normalized, "/bar.txt");
}

#[test]
fn normalize_path_handles_root_only() {
    let normalized = normalize_path(Path::new("/"));
    assert_eq!(normalized, "/");
}

#[test]
fn normalize_path_handles_root_with_current_dir() {
    let normalized = normalize_path(Path::new("/."));
    assert_eq!(normalized, "/");
}

#[test]
fn normalize_path_handles_root_with_parent_dir() {
    let normalized = normalize_path(Path::new("/.."));
    assert_eq!(normalized, "/");
}

#[test]
fn normalize_path_handles_trailing_slash_in_relative() {
    // PathBuf normalizes away trailing slashes on most platforms
    let path = PathBuf::from("foo/bar/");
    let normalized = normalize_path(&path);
    // After PathBuf parsing, trailing slash is typically removed
    assert_eq!(normalized, "foo/bar");
}

#[test]
fn normalize_path_handles_trailing_slash_in_absolute() {
    let path = PathBuf::from("/foo/bar/");
    let normalized = normalize_path(&path);
    assert_eq!(normalized, "/foo/bar");
}

#[test]
fn normalize_path_handles_empty_components() {
    let normalized = normalize_path(Path::new("foo//bar"));
    // Path parsing collapses empty components
    assert_eq!(normalized, "foo/bar");
}

#[cfg(unix)]
#[test]
fn normalize_path_handles_symlink_like_paths() {
    // This test doesn't actually create symlinks, just verifies
    // that paths that might be symlinks are normalized correctly
    let normalized = normalize_path(Path::new("/usr/local/../bin/tool"));
    assert_eq!(normalized, "/usr/bin/tool");
}

#[cfg(unix)]
#[test]
fn normalize_path_preserves_path_without_following_symlinks() {
    // normalize_path does NOT follow symlinks, it just normalizes the string
    // If /link points to /target, normalize_path("/link/file") returns "/link/file"
    let normalized = normalize_path(Path::new("/link/file"));
    assert_eq!(normalized, "/link/file");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_windows_backslashes() {
    let normalized = normalize_path(Path::new(r"foo\bar\baz.txt"));
    assert_eq!(normalized, "foo/bar/baz.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_windows_mixed_slashes() {
    let normalized = normalize_path(Path::new(r"foo\bar/baz\qux.txt"));
    assert_eq!(normalized, "foo/bar/baz/qux.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_windows_drive_letter() {
    let normalized = normalize_path(Path::new(r"C:\Users\test\file.txt"));
    assert_eq!(normalized, "C:/Users/test/file.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_windows_drive_with_parent() {
    let normalized = normalize_path(Path::new(r"C:\Users\test\..\other\file.txt"));
    assert_eq!(normalized, "C:/Users/other/file.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_windows_drive_with_current() {
    let normalized = normalize_path(Path::new(r"C:\Users\.\test\.\file.txt"));
    assert_eq!(normalized, "C:/Users/test/file.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_preserves_uppercase_drive_letter() {
    let normalized = normalize_path(Path::new(r"c:\users\test\file.txt"));
    // Drive letters are normalized to uppercase
    assert_eq!(normalized, "C:/users/test/file.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_unc_path() {
    let normalized = normalize_path(Path::new(r"\\server\share\dir\file.txt"));
    assert_eq!(normalized, "//server/share/dir/file.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_unc_with_parent() {
    let normalized = normalize_path(Path::new(r"\\server\share\dir\..\file.txt"));
    assert_eq!(normalized, "//server/share/file.txt");
}

#[cfg(windows)]
#[test]
fn normalize_path_handles_verbatim_path() {
    let normalized = normalize_path(Path::new(r"\\?\C:\Users\test\file.txt"));
    assert_eq!(normalized, "C:/Users/test/file.txt");
}

#[test]
fn normalize_path_handles_deep_nesting() {
    let normalized = normalize_path(Path::new("a/b/c/d/e/f/g/h/i/j/file.txt"));
    assert_eq!(normalized, "a/b/c/d/e/f/g/h/i/j/file.txt");
}

#[test]
fn normalize_path_handles_deep_parent_collapse() {
    let normalized = normalize_path(Path::new("a/b/c/d/e/../../../../file.txt"));
    assert_eq!(normalized, "a/file.txt");
}

#[test]
fn normalize_path_handles_alternating_parent_and_normal() {
    let normalized = normalize_path(Path::new("a/../b/../c/../d/file.txt"));
    assert_eq!(normalized, "d/file.txt");
}

#[test]
fn normalize_path_handles_unicode_in_components() {
    let normalized = normalize_path(Path::new("foo/カフェ/файл.txt"));
    assert_eq!(normalized, "foo/カフェ/файл.txt");
}

#[test]
fn normalize_path_handles_spaces_in_components() {
    let normalized = normalize_path(Path::new("foo/bar baz/qux file.txt"));
    assert_eq!(normalized, "foo/bar baz/qux file.txt");
}

#[test]
fn normalize_path_handles_special_chars_in_components() {
    let normalized = normalize_path(Path::new("foo/bar-baz_qux/file@host.txt"));
    assert_eq!(normalized, "foo/bar-baz_qux/file@host.txt");
}

#[test]
fn normalize_path_handles_dots_in_filename() {
    let normalized = normalize_path(Path::new("foo/bar/file.tar.gz"));
    assert_eq!(normalized, "foo/bar/file.tar.gz");
}

#[test]
fn normalize_path_handles_hidden_file() {
    let normalized = normalize_path(Path::new("foo/bar/.hidden"));
    assert_eq!(normalized, "foo/bar/.hidden");
}

#[test]
fn normalize_path_distinguishes_hidden_from_current() {
    let normalized = normalize_path(Path::new("foo/./.hidden"));
    assert_eq!(normalized, "foo/.hidden");
}

#[test]
fn normalize_path_handles_double_dots_in_filename() {
    let normalized = normalize_path(Path::new("foo/bar/..hidden"));
    assert_eq!(normalized, "foo/bar/..hidden");
}

#[test]
fn normalize_path_complex_real_world_example() {
    let normalized = normalize_path(Path::new("./src/../tests/./integration/../unit/test.rs"));
    assert_eq!(normalized, "tests/unit/test.rs");
}

#[cfg(unix)]
#[test]
fn normalize_path_handles_absolute_complex_unix() {
    let normalized = normalize_path(Path::new("/home/user/../other/./project/src/../lib/file.rs"));
    assert_eq!(normalized, "/home/other/project/lib/file.rs");
}

#[test]
fn normalize_path_handles_only_parent_dirs() {
    let normalized = normalize_path(Path::new(".."));
    assert_eq!(normalized, "..");
}

#[test]
fn normalize_path_handles_only_multiple_parent_dirs() {
    let normalized = normalize_path(Path::new("../../.."));
    assert_eq!(normalized, "../../..");
}

#[test]
fn normalize_path_handles_parent_with_trailing_current() {
    let normalized = normalize_path(Path::new("../foo/."));
    assert_eq!(normalized, "../foo");
}

#[test]
fn append_normalized_os_str_handles_forward_slashes() {
    let mut buffer = String::new();
    append_normalized_os_str(&mut buffer, OsStr::new("foo/bar/baz"));
    assert_eq!(buffer, "foo/bar/baz");
}

#[test]
fn append_normalized_os_str_handles_no_backslashes() {
    let mut buffer = String::new();
    append_normalized_os_str(&mut buffer, OsStr::new("simple"));
    assert_eq!(buffer, "simple");
}

#[test]
fn append_normalized_os_str_handles_empty_string() {
    let mut buffer = String::new();
    append_normalized_os_str(&mut buffer, OsStr::new(""));
    assert_eq!(buffer, "");
}

#[test]
fn append_normalized_os_str_appends_to_existing_content() {
    let mut buffer = String::from("prefix/");
    append_normalized_os_str(&mut buffer, OsStr::new("suffix"));
    assert_eq!(buffer, "prefix/suffix");
}

#[test]
fn append_normalized_os_str_converts_multiple_backslashes() {
    let mut buffer = String::new();
    append_normalized_os_str(&mut buffer, OsStr::new("a\\b\\c\\d"));
    assert_eq!(buffer, "a/b/c/d");
}

#[test]
fn canonicalize_or_fallback_returns_original_on_nonexistent() {
    let nonexistent = Path::new("/this/path/does/not/exist/hopefully");
    let result = canonicalize_or_fallback(nonexistent);
    assert_eq!(result, nonexistent.to_path_buf());
}

#[test]
fn canonicalize_or_fallback_handles_existing_path() {
    // Use the manifest directory which we know exists
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let result = canonicalize_or_fallback(manifest_dir);
    // The result should be an absolute path
    assert!(result.is_absolute());
}

#[test]
fn strip_normalized_workspace_prefix_handles_empty_suffix() {
    let root = "/workspace/project";
    let path = "/workspace/project";
    let stripped = strip_normalized_workspace_prefix(path, root)
        .expect("exact match should return current directory");
    assert_eq!(stripped, ".");
}

#[test]
fn strip_normalized_workspace_prefix_handles_nested_path() {
    let root = "/workspace";
    let path = "/workspace/crates/core/src/lib.rs";
    let stripped = strip_normalized_workspace_prefix(path, root)
        .expect("nested path should be stripped");
    assert_eq!(stripped, "crates/core/src/lib.rs");
}

#[test]
fn strip_normalized_workspace_prefix_rejects_unrelated_path() {
    let root = "/workspace/project";
    let path = "/other/project/file.rs";
    assert!(strip_normalized_workspace_prefix(path, root).is_none());
}

#[test]
fn strip_normalized_workspace_prefix_handles_root_with_trailing_slash() {
    let root = "/workspace/";
    let path = "/workspace/src/lib.rs";
    let stripped = strip_normalized_workspace_prefix(path, root)
        .expect("trailing slash on root should work");
    assert_eq!(stripped, "src/lib.rs");
}

#[test]
fn strip_normalized_workspace_prefix_handles_path_with_trailing_slash() {
    let root = "/workspace";
    let path = "/workspace/";
    let stripped = strip_normalized_workspace_prefix(path, root)
        .expect("path with trailing slash should return current dir");
    assert_eq!(stripped, ".");
}

#[test]
fn strip_normalized_workspace_prefix_rejects_similar_prefix() {
    let root = "/workspace";
    let path = "/workspace-backup/src/lib.rs";
    assert!(
        strip_normalized_workspace_prefix(path, root).is_none(),
        "should not match similar but different prefix"
    );
}

#[cfg(windows)]
#[test]
fn strip_normalized_workspace_prefix_handles_windows_paths() {
    let root = "C:/workspace";
    let path = "C:/workspace/crates/core/src/lib.rs";
    let stripped = strip_normalized_workspace_prefix(path, root)
        .expect("windows path should be stripped");
    assert_eq!(stripped, "crates/core/src/lib.rs");
}

#[test]
fn normalize_path_roundtrip_with_strip_prefix() {
    let workspace = Path::new("/home/user/project");
    let file_path = workspace.join("src/lib.rs");

    let normalized_workspace = normalize_path(workspace);
    let normalized_file = normalize_path(&file_path);

    let stripped = strip_normalized_workspace_prefix(&normalized_file, &normalized_workspace)
        .expect("normalized paths should strip correctly");

    assert_eq!(stripped, "src/lib.rs");
}
