//! Integration tests for the `O_TMPFILE` / anonymous temp file write path.
//!
//! These tests verify that the anonymous temp file strategy (Linux `O_TMPFILE`)
//! works correctly as an alternative to named temp files for atomic writes.
//! On non-Linux platforms, the tests verify graceful fallback.

use std::fs;
use std::io::Write;
use std::path::Path;

use engine::local_copy::{DestinationWriteGuard, remove_existing_destination};
use fast_io::o_tmpfile::{
    AnonymousTempFile, OTmpfileSupport, TempFileResult, o_tmpfile_probe, open_temp_file,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Cross-platform probe tests
// ---------------------------------------------------------------------------

#[test]
fn probe_returns_valid_result_for_tempdir() {
    let dir = tempdir().expect("tempdir");
    let result = o_tmpfile_probe(dir.path());
    assert!(result == OTmpfileSupport::Available || result == OTmpfileSupport::Unavailable);
}

#[test]
fn probe_returns_unavailable_for_missing_directory() {
    let result = o_tmpfile_probe(Path::new("/no_such_directory_for_o_tmpfile_test"));
    assert_eq!(result, OTmpfileSupport::Unavailable);
}

#[test]
fn open_temp_file_returns_result_for_tempdir() {
    let dir = tempdir().expect("tempdir");
    let result = open_temp_file(dir.path());
    match result {
        TempFileResult::Anonymous(_) => {
            // Verify the directory is still empty (anonymous = no visible entry).
            let entries: Vec<_> = fs::read_dir(dir.path()).expect("read_dir").collect();
            assert!(
                entries.is_empty(),
                "anonymous file must not appear in directory listing"
            );
        }
        TempFileResult::Unavailable => {
            // Expected on non-Linux or unsupported fs.
        }
    }
}

// ---------------------------------------------------------------------------
// Named temp file fallback (always works on all platforms)
// ---------------------------------------------------------------------------

#[test]
fn named_temp_file_fallback_write_and_commit() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("output.txt");

    let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
    file.write_all(b"fallback content").expect("write");
    drop(file);

    // The staging (named temp) file should be visible before commit.
    let staging = guard.staging_path().to_path_buf();
    assert!(staging.exists(), "named temp file must be visible");

    guard.commit().expect("commit");

    assert!(dest.exists());
    assert!(!staging.exists());
    assert_eq!(fs::read_to_string(&dest).expect("read"), "fallback content");
}

#[test]
fn named_temp_file_cleanup_on_drop() {
    let dir = tempdir().expect("tempdir");
    let dest = dir.path().join("output.txt");

    let staging;
    {
        let (guard, _file) = DestinationWriteGuard::new(&dest, false, None, None).expect("guard");
        staging = guard.staging_path().to_path_buf();
    }

    assert!(
        !staging.exists(),
        "named temp file must be cleaned up on drop"
    );
    assert!(
        !dest.exists(),
        "destination must not exist after drop without commit"
    );
}

// ---------------------------------------------------------------------------
// Linux-specific O_TMPFILE tests
// ---------------------------------------------------------------------------

#[cfg(target_os = "linux")]
mod linux {
    use super::*;

    /// Skips the test if O_TMPFILE is not supported on the temp directory's filesystem.
    fn require_o_tmpfile(dir: &Path) -> bool {
        o_tmpfile_probe(dir) == OTmpfileSupport::Available
    }

    #[test]
    fn anonymous_file_is_invisible_during_write() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
        atf.file_mut().write_all(b"invisible data").expect("write");

        let count = fs::read_dir(dir.path()).expect("read_dir").count();
        assert_eq!(count, 0, "anonymous file must not appear in directory");
    }

    #[test]
    fn anonymous_file_linked_to_destination_has_correct_content() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let content = b"hello from O_TMPFILE";
        let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
        atf.file_mut().write_all(content).expect("write");

        let dest = dir.path().join("linked.txt");
        atf.link_to(&dest).expect("link_to");

        assert!(dest.exists());
        assert_eq!(fs::read(&dest).expect("read"), content);
    }

    #[test]
    fn anonymous_file_large_write_integrity() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        // 2 MB of patterned data.
        let size = 2 * 1024 * 1024;
        let pattern: Vec<u8> = (0..=255u8).cycle().take(size).collect();

        let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
        atf.file_mut().write_all(&pattern).expect("write");

        let dest = dir.path().join("large.bin");
        atf.link_to(&dest).expect("link_to");

        let data = fs::read(&dest).expect("read");
        assert_eq!(data.len(), size);
        assert_eq!(data, pattern);
    }

    #[test]
    fn anonymous_file_drop_without_link_leaves_no_orphan() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        {
            let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
            atf.file_mut()
                .write_all(b"will be orphaned")
                .expect("write");
        }

        let count = fs::read_dir(dir.path()).expect("read_dir").count();
        assert_eq!(count, 0, "no orphaned files after drop without link");
    }

    #[test]
    fn anonymous_file_link_fails_when_dest_already_exists() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let dest = dir.path().join("existing.txt");
        fs::write(&dest, b"pre-existing").expect("create existing");

        let atf = AnonymousTempFile::open(dir.path()).expect("open");
        let err = atf.link_to(&dest).expect_err("link_to should fail");
        assert_eq!(
            err.kind(),
            std::io::ErrorKind::AlreadyExists,
            "should get AlreadyExists when destination file exists"
        );
    }

    #[test]
    fn anonymous_file_link_after_remove_existing() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let dest = dir.path().join("replace.txt");
        fs::write(&dest, b"old content").expect("create existing");

        let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
        atf.file_mut().write_all(b"new content").expect("write");

        // Remove existing, then link - mirrors DestinationWriteGuard::commit() pattern.
        remove_existing_destination(&dest).expect("remove existing");
        atf.link_to(&dest).expect("link_to after remove");

        assert_eq!(fs::read_to_string(&dest).expect("read"), "new content");
    }

    #[test]
    fn anonymous_file_link_fails_for_nonexistent_parent() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let atf = AnonymousTempFile::open(dir.path()).expect("open");
        let bad_dest = dir.path().join("no_such_dir").join("file.txt");
        let result = atf.link_to(&bad_dest);
        assert!(
            result.is_err(),
            "link_to should fail for nonexistent parent dir"
        );
    }

    #[test]
    fn anonymous_file_into_file_is_usable() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let atf = AnonymousTempFile::open(dir.path()).expect("open");
        let mut file = atf.into_file();
        file.write_all(b"after into_file").expect("write");
    }

    #[test]
    fn open_temp_file_returns_anonymous_on_supported_fs() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let result = open_temp_file(dir.path());
        assert!(
            matches!(result, TempFileResult::Anonymous(_)),
            "open_temp_file should return Anonymous on O_TMPFILE-capable fs"
        );
    }

    #[test]
    fn open_temp_file_anonymous_full_workflow() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        if let TempFileResult::Anonymous(mut atf) = open_temp_file(dir.path()) {
            atf.file_mut().write_all(b"full workflow").expect("write");
            let dest = dir.path().join("workflow.txt");
            atf.link_to(&dest).expect("link");
            assert_eq!(fs::read_to_string(&dest).expect("read"), "full workflow");
        } else {
            panic!("expected Anonymous result");
        }
    }

    #[test]
    fn multiple_anonymous_files_in_same_directory() {
        let dir = tempdir().expect("tempdir");
        if !require_o_tmpfile(dir.path()) {
            return;
        }

        let mut files = Vec::new();
        for i in 0..5 {
            let mut atf = AnonymousTempFile::open(dir.path()).expect("open");
            atf.file_mut()
                .write_all(format!("file_{i}").as_bytes())
                .expect("write");
            files.push(atf);
        }

        let count = fs::read_dir(dir.path()).expect("read_dir").count();
        assert_eq!(count, 0, "all files should be invisible");

        for (i, atf) in files.into_iter().enumerate() {
            let dest = dir.path().join(format!("file_{i}.txt"));
            atf.link_to(&dest).expect("link_to");
        }

        for i in 0..5 {
            let dest = dir.path().join(format!("file_{i}.txt"));
            let content = fs::read_to_string(&dest).expect("read");
            assert_eq!(content, format!("file_{i}"));
        }
    }
}

// ---------------------------------------------------------------------------
// Non-Linux fallback tests
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "linux"))]
mod non_linux {
    use super::*;

    #[test]
    fn probe_always_unavailable() {
        let dir = tempdir().expect("tempdir");
        assert_eq!(o_tmpfile_probe(dir.path()), OTmpfileSupport::Unavailable);
    }

    #[test]
    fn anonymous_temp_file_open_returns_unsupported() {
        let dir = tempdir().expect("tempdir");
        let err = AnonymousTempFile::open(dir.path()).expect_err("should fail on non-Linux");
        assert_eq!(err.kind(), std::io::ErrorKind::Unsupported);
    }

    #[test]
    fn open_temp_file_returns_unavailable() {
        let dir = tempdir().expect("tempdir");
        assert!(matches!(
            open_temp_file(dir.path()),
            TempFileResult::Unavailable
        ));
    }
}
