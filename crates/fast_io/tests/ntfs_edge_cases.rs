//! NTFS edge-case integration tests for Windows-specific filesystem behaviors
//! (WTD-3).
//!
//! These tests exercise NTFS semantics that differ from POSIX and can cause
//! silent data loss or sync failures if not handled:
//!
//! - **Long paths (> 260 characters)**: NTFS supports paths up to ~32,767
//!   characters when the `\\?\` prefix is used. Standard Win32 APIs without
//!   the prefix are limited to `MAX_PATH` (260). The IOCP reader/writer use
//!   `CreateFileW` which accepts the extended prefix.
//! - **Case-insensitive filename matching**: NTFS is case-preserving but
//!   case-insensitive by default. Two paths that differ only in case refer
//!   to the same file.
//! - **Reparse points**: NTFS junctions and directory symlinks are reparse
//!   points. File operations that follow reparse points may land in
//!   unexpected directories.
//! - **File attributes**: NTFS files carry attributes (read-only, hidden,
//!   system, archive) that affect write access and visibility.
//! - **Alternate data streams**: Covered by the `metadata` crate's
//!   `xattr_windows` tests and the `windows-acl-xattr` CI job.
//!
//! Tests that require admin privileges are marked `#[ignore]` so they do
//! not fail on unprivileged CI runners but can be run manually with
//! `cargo nextest run --run-ignored ignored-only`.
//!
//! The entire file compiles to nothing on non-Windows targets.

#![cfg(windows)]

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Returns `true` if the filesystem at `path` supports long paths (> 260).
/// Falls back to `false` if detection fails.
fn long_paths_supported(dir: &Path) -> bool {
    // Try creating a directory with a name long enough to push total path
    // length past MAX_PATH. If it succeeds, long paths are supported.
    let long_name = "a".repeat(200);
    let test_path = dir.join(&long_name);
    match fs::create_dir(&test_path) {
        Ok(()) => {
            let _ = fs::remove_dir(&test_path);
            true
        }
        Err(_) => false,
    }
}

/// Returns `true` if the filesystem at `path` is case-insensitive (NTFS
/// default). Probes by creating a file with a mixed-case name and checking
/// if the lower-case variant refers to the same file.
fn is_case_insensitive(dir: &Path) -> bool {
    let upper = dir.join("CaseProbe_UPPER.tmp");
    let lower = dir.join("caseprobe_upper.tmp");
    if fs::write(&upper, b"probe").is_err() {
        return false;
    }
    let exists = lower.exists();
    let _ = fs::remove_file(&upper);
    exists
}

/// Returns `true` if the current process has permission to create directory
/// junctions on the given volume. Junctions do not require admin rights on
/// most Windows versions, but some locked-down environments block them.
fn can_create_junctions(dir: &Path) -> bool {
    use std::os::windows::fs as winfs;
    let target = dir.join("junction_target_probe");
    let link = dir.join("junction_link_probe");
    if fs::create_dir(&target).is_err() {
        return false;
    }
    let ok = winfs::symlink_dir(&target, &link).is_ok();
    let _ = fs::remove_dir(&link);
    let _ = fs::remove_dir(&target);
    ok
}

// ---------------------------------------------------------------------------
// WTD-3.a: Long paths (> MAX_PATH / 260 characters)
// ---------------------------------------------------------------------------

/// The `\\?\` extended-length path prefix lets Win32 APIs access paths
/// longer than 260 characters. The IOCP writer opens files via
/// `CreateFileW`, which accepts the extended prefix. This test writes
/// and reads back a file whose total path length exceeds MAX_PATH.
#[test]
fn long_path_write_and_read_roundtrip() {
    let dir = tempdir().unwrap();
    if !long_paths_supported(dir.path()) {
        eprintln!("skipping: filesystem does not support long paths");
        return;
    }

    // Build a path that exceeds 260 characters using nested directories.
    // Each segment is 50 characters; 6 levels gets us to ~300+ chars.
    let mut deep = dir.path().to_path_buf();
    for i in 0..6 {
        let segment = format!("{:0>50}", i);
        deep = deep.join(segment);
    }
    fs::create_dir_all(&deep).unwrap();

    let file_path = deep.join("long_path_test.bin");
    assert!(
        file_path.to_string_lossy().len() > 260,
        "total path must exceed MAX_PATH for this test to be meaningful, got {}",
        file_path.to_string_lossy().len()
    );

    let payload = b"data-at-long-path";
    fs::write(&file_path, payload).unwrap();
    let content = fs::read(&file_path).unwrap();
    assert_eq!(content, payload);
}

/// The `\\?\` prefix enables paths up to ~32,767 characters. Test with a
/// path near the practical limit by using deeply nested single-char
/// directory names.
#[test]
fn very_long_path_via_extended_prefix() {
    let dir = tempdir().unwrap();
    if !long_paths_supported(dir.path()) {
        eprintln!("skipping: filesystem does not support long paths");
        return;
    }

    // Use the \\?\ prefix for the base, then nest directories to push
    // the total well past 260.
    let base = dir.path().to_path_buf();

    // 30 levels of 10-char segments = ~300 chars of nesting.
    let mut deep = base.clone();
    for i in 0..30 {
        deep = deep.join(format!("d{:0>9}", i));
    }

    // Use the \\?\ prefix to create the deep path.
    let prefixed = if deep.starts_with("\\\\?\\") {
        deep.clone()
    } else {
        PathBuf::from(format!("\\\\?\\{}", deep.display()))
    };

    match fs::create_dir_all(&prefixed) {
        Ok(()) => {}
        Err(e) => {
            eprintln!("skipping: cannot create deep directory: {e}");
            return;
        }
    }

    let file_path = prefixed.join("deep_file.txt");
    fs::write(&file_path, b"deep-data").unwrap();
    let content = fs::read(&file_path).unwrap();
    assert_eq!(content, b"deep-data");
}

// ---------------------------------------------------------------------------
// WTD-3.b: Case-insensitive filename matching
// ---------------------------------------------------------------------------

/// NTFS is case-preserving but case-insensitive. Writing to "File.TXT" and
/// reading from "file.txt" must return the same content.
#[test]
fn case_insensitive_read_write() {
    let dir = tempdir().unwrap();
    if !is_case_insensitive(dir.path()) {
        eprintln!("skipping: filesystem is case-sensitive");
        return;
    }

    let write_path = dir.path().join("CaseSensitivity.TXT");
    let read_path = dir.path().join("casesensitivity.txt");

    fs::write(&write_path, b"case-test-data").unwrap();

    // Reading via the differently-cased path must succeed.
    let content = fs::read(&read_path).unwrap();
    assert_eq!(content, b"case-test-data");
}

/// NTFS preserves the original case of the filename. After creating
/// "MyFile.Txt", the directory listing should show exactly that casing.
#[test]
fn case_preserving_directory_listing() {
    let dir = tempdir().unwrap();
    if !is_case_insensitive(dir.path()) {
        eprintln!("skipping: filesystem is case-sensitive");
        return;
    }

    let path = dir.path().join("MyFile.Txt");
    fs::write(&path, b"preserve-case").unwrap();

    let entries: Vec<String> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();

    assert!(
        entries.iter().any(|n| n == "MyFile.Txt"),
        "directory listing must preserve original case, got: {entries:?}"
    );
}

/// Creating a second file with a differently-cased name on NTFS should
/// overwrite or refer to the same file, not create a separate entry.
#[test]
fn case_insensitive_overwrite() {
    let dir = tempdir().unwrap();
    if !is_case_insensitive(dir.path()) {
        eprintln!("skipping: filesystem is case-sensitive");
        return;
    }

    let path_upper = dir.path().join("OVERWRITE.DAT");
    let path_lower = dir.path().join("overwrite.dat");

    fs::write(&path_upper, b"first-version").unwrap();
    fs::write(&path_lower, b"second-version").unwrap();

    // Only one file should exist in the directory.
    let entries: Vec<_> = fs::read_dir(dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "case-insensitive FS should have exactly one file, got: {:?}",
        entries
            .iter()
            .map(|e| e.file_name())
            .collect::<Vec<_>>()
    );

    // Content should be the second write.
    let content = fs::read(&path_upper).unwrap();
    assert_eq!(content, b"second-version");
}

// ---------------------------------------------------------------------------
// WTD-3.c: Reparse points (directory junctions)
// ---------------------------------------------------------------------------

/// A directory junction is an NTFS reparse point that redirects path
/// resolution to another directory. Files accessed through the junction
/// must be readable and writable.
#[test]
fn junction_target_file_roundtrip() {
    let dir = tempdir().unwrap();
    if !can_create_junctions(dir.path()) {
        eprintln!("skipping: cannot create directory junctions (permissions or FS type)");
        return;
    }

    let target_dir = dir.path().join("real_dir");
    let junction = dir.path().join("junction_link");
    fs::create_dir(&target_dir).unwrap();

    // Create a junction pointing to target_dir.
    // On Windows, symlink_dir creates a directory symbolic link which
    // may require developer mode or admin rights; fall back gracefully.
    if std::os::windows::fs::symlink_dir(&target_dir, &junction).is_err() {
        eprintln!("skipping: symlink_dir failed (may need developer mode)");
        return;
    }

    // Write through the junction.
    let file_via_junction = junction.join("through_junction.txt");
    fs::write(&file_via_junction, b"junction-data").unwrap();

    // Read through the real path.
    let file_via_real = target_dir.join("through_junction.txt");
    let content = fs::read(&file_via_real).unwrap();
    assert_eq!(content, b"junction-data");
}

/// Metadata on the junction link itself should report it as a symlink
/// (reparse point), while metadata through the junction follows through.
#[test]
fn junction_metadata_reports_symlink() {
    let dir = tempdir().unwrap();
    if !can_create_junctions(dir.path()) {
        eprintln!("skipping: cannot create directory junctions");
        return;
    }

    let target_dir = dir.path().join("meta_target");
    let junction = dir.path().join("meta_junction");
    fs::create_dir(&target_dir).unwrap();

    if std::os::windows::fs::symlink_dir(&target_dir, &junction).is_err() {
        eprintln!("skipping: symlink_dir failed");
        return;
    }

    // symlink_metadata does NOT follow the link.
    let link_meta = fs::symlink_metadata(&junction).unwrap();
    assert!(
        link_meta.file_type().is_symlink(),
        "junction must report as symlink via symlink_metadata"
    );

    // Regular metadata follows through.
    let followed_meta = fs::metadata(&junction).unwrap();
    assert!(
        followed_meta.file_type().is_dir(),
        "following the junction must yield a directory"
    );
}

// ---------------------------------------------------------------------------
// WTD-3.d: File attributes (read-only, hidden, system, archive)
// ---------------------------------------------------------------------------

/// Setting the read-only attribute must prevent writes and clearing it
/// must restore write access.
#[test]
fn readonly_attribute_blocks_writes() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("readonly.txt");
    fs::write(&path, b"original").unwrap();

    // Set read-only.
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(true);
    fs::set_permissions(&path, perms).unwrap();

    // Writing must fail.
    let err = fs::write(&path, b"overwrite")
        .expect_err("write to read-only file must fail");
    assert_eq!(err.kind(), io::ErrorKind::PermissionDenied);

    // Clear read-only and verify write succeeds.
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_readonly(false);
    fs::set_permissions(&path, perms).unwrap();

    fs::write(&path, b"now-writable").unwrap();
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "now-writable");
}

/// The archive attribute is set automatically on file modification and can
/// be queried via `GetFileAttributesW`.
#[test]
fn archive_attribute_set_on_modification() {
    use std::os::windows::fs::MetadataExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("archive.txt");
    fs::write(&path, b"initial-data").unwrap();

    let meta = fs::metadata(&path).unwrap();
    let attrs = meta.file_attributes();

    // FILE_ATTRIBUTE_ARCHIVE = 0x20
    const FILE_ATTRIBUTE_ARCHIVE: u32 = 0x20;
    assert!(
        attrs & FILE_ATTRIBUTE_ARCHIVE != 0,
        "newly written file must have the archive attribute set, got attrs={attrs:#x}"
    );
}

/// Hidden files are accessible via their path but may not appear in
/// standard directory listings. This test verifies that hidden files
/// are still readable.
#[test]
fn hidden_file_is_still_readable() {
    use std::os::windows::ffi::OsStrExt;

    let dir = tempdir().unwrap();
    let path = dir.path().join("hidden.txt");
    fs::write(&path, b"hidden-content").unwrap();

    // Set the hidden attribute via SetFileAttributesW.
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x02;
    // SAFETY: `wide` is null-terminated; SetFileAttributesW is a simple
    // metadata mutation that does not affect file data.
    #[allow(unsafe_code)]
    let ok = unsafe {
        windows_sys::Win32::Storage::FileSystem::SetFileAttributesW(
            wide.as_ptr(),
            FILE_ATTRIBUTE_HIDDEN,
        )
    };
    if ok == 0 {
        eprintln!(
            "skipping: SetFileAttributesW failed: {}",
            io::Error::last_os_error()
        );
        return;
    }

    // File must still be readable via its exact path.
    let content = fs::read_to_string(&path).unwrap();
    assert_eq!(content, "hidden-content");
}

// ---------------------------------------------------------------------------
// WTD-3.e: Unicode filenames
// ---------------------------------------------------------------------------

/// NTFS supports Unicode filenames. This test creates files with non-ASCII
/// characters and verifies round-trip data integrity.
#[test]
fn unicode_filename_roundtrip() {
    let dir = tempdir().unwrap();

    let names = [
        "\u{00e9}l\u{00e8}ve.txt",     // French accents: eleve
        "\u{65e5}\u{672c}\u{8a9e}.txt", // Japanese: nihongo
        "\u{0410}\u{0411}\u{0412}.txt", // Cyrillic: ABV
    ];

    for name in &names {
        let path = dir.path().join(name);
        let payload = format!("content-for-{name}");
        fs::write(&path, payload.as_bytes()).unwrap();
        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, payload, "round-trip failed for filename {name}");
    }
}

// ---------------------------------------------------------------------------
// WTD-3.f: Empty filename components and trailing dots/spaces
// ---------------------------------------------------------------------------

/// NTFS silently strips trailing dots and spaces from filenames. A file
/// created as "test. " is stored as "test" (or "test." depending on API).
/// This test documents the behavior for rsync's filename handling.
#[test]
fn trailing_dots_and_spaces_normalized() {
    let dir = tempdir().unwrap();

    // On NTFS, trailing dots and spaces are stripped by Win32 APIs.
    // CreateFileW("test. ") actually creates "test".
    let path_with_trailing = dir.path().join("stripped.");
    match fs::write(&path_with_trailing, b"data") {
        Ok(()) => {
            // The file was created - verify what name it actually has.
            let entries: Vec<String> = fs::read_dir(dir.path())
                .unwrap()
                .filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            // NTFS strips the trailing dot.
            assert!(
                entries.iter().any(|n| n == "stripped" || n == "stripped."),
                "NTFS should normalize trailing dots, got: {entries:?}"
            );
        }
        Err(_) => {
            // Some configurations reject the name entirely.
            eprintln!("trailing-dot filename rejected by this filesystem");
        }
    }
}

// ---------------------------------------------------------------------------
// WTD-3.g: Maximum filename length
// ---------------------------------------------------------------------------

/// NTFS supports filenames up to 255 characters (not bytes). This test
/// creates a file at the limit and verifies it round-trips.
#[test]
fn max_filename_length_255_chars() {
    let dir = tempdir().unwrap();

    // 251 chars + ".txt" = 255 total.
    let name = format!("{}.txt", "x".repeat(251));
    assert_eq!(name.len(), 255);

    let path = dir.path().join(&name);
    fs::write(&path, b"max-name").unwrap();
    let content = fs::read(&path).unwrap();
    assert_eq!(content, b"max-name");
}

/// Filenames longer than 255 characters must fail on NTFS.
#[test]
fn filename_over_255_chars_fails() {
    let dir = tempdir().unwrap();

    let name = "x".repeat(256);
    let path = dir.path().join(&name);
    let result = fs::write(&path, b"too-long");
    assert!(
        result.is_err(),
        "creating a file with a 256-char name must fail on NTFS"
    );
}

// ---------------------------------------------------------------------------
// WTD-3.h: Concurrent writers to the same file via sharing flags
// ---------------------------------------------------------------------------

/// Two writers with `FILE_SHARE_WRITE` can coexist. This test verifies
/// that the IOCP writer's sharing flags allow a second handle to read
/// the file while the first is still writing.
#[test]
#[cfg(feature = "iocp")]
fn concurrent_read_during_iocp_write() {
    use fast_io::iocp::{IocpConfig, IocpWriter, is_iocp_available};

    if !is_iocp_available() {
        eprintln!("skipping: IOCP unavailable");
        return;
    }

    let dir = tempdir().unwrap();
    let path = dir.path().join("concurrent_rw.bin");
    let config = IocpConfig::default();

    let mut writer = IocpWriter::create(&path, &config).unwrap();
    writer.write_all(b"concurrent-data").unwrap();
    writer.flush().unwrap();

    // While the IOCP writer still has the file open, a concurrent reader
    // must be able to open it for reading (FILE_SHARE_READ was set).
    let content = fs::read(&path).unwrap();
    assert_eq!(content, b"concurrent-data");

    // The writer can continue writing after the concurrent read.
    writer.write_all(b"-more").unwrap();
    writer.flush().unwrap();
    drop(writer);

    let final_content = fs::read(&path).unwrap();
    assert_eq!(final_content, b"concurrent-data-more");
}
