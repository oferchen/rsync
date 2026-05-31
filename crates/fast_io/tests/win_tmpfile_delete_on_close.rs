//! Integration tests for the Windows `FILE_FLAG_DELETE_ON_CLOSE` temp file path.
//!
//! These tests verify that the delete-on-close temp file strategy works
//! correctly as an alternative to named temp files for auto-cleanup writes.
//! On non-Windows platforms, the tests verify graceful fallback.

use std::path::Path;

use fast_io::win_tmpfile::{
    WinDeleteOnCloseSupport, WinTempFileResult, WindowsTempFile, open_win_temp_file,
    win_tmpfile_probe,
};
use tempfile::tempdir;

// ---------------------------------------------------------------------------
// Cross-platform probe tests
// ---------------------------------------------------------------------------

#[test]
fn probe_returns_valid_result_for_tempdir() {
    let dir = tempdir().expect("tempdir");
    let result = win_tmpfile_probe(dir.path());
    assert!(
        result == WinDeleteOnCloseSupport::Available
            || result == WinDeleteOnCloseSupport::Unavailable
    );
}

#[test]
fn probe_returns_unavailable_for_missing_directory() {
    let result = win_tmpfile_probe(Path::new("/no_such_directory_for_win_tmpfile_test_9999"));
    assert_eq!(result, WinDeleteOnCloseSupport::Unavailable);
}

#[test]
fn open_win_temp_file_returns_result_for_tempdir() {
    let dir = tempdir().expect("tempdir");
    let result = open_win_temp_file(dir.path());
    match result {
        WinTempFileResult::DeleteOnClose(wtf) => {
            // Delete-on-close files are visible (unlike O_TMPFILE).
            let count = std::fs::read_dir(dir.path()).expect("read_dir").count();
            assert!(count >= 1, "delete-on-close file should be visible");
            drop(wtf);
        }
        WinTempFileResult::Unavailable => {
            // Expected on non-Windows.
        }
    }
}

// ---------------------------------------------------------------------------
// Windows-specific tests
// ---------------------------------------------------------------------------

#[cfg(target_os = "windows")]
mod windows {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn temp_file_deleted_on_drop() {
        let dir = tempdir().expect("tempdir");
        let wtf = WindowsTempFile::open(dir.path()).expect("open");
        let path = wtf.temp_path().to_path_buf();
        assert!(path.exists());
        drop(wtf);
        assert!(
            !path.exists(),
            "temp file must be deleted when handle is dropped"
        );
    }

    #[test]
    fn temp_file_write_and_commit() {
        let dir = tempdir().expect("tempdir");
        let mut wtf = WindowsTempFile::open(dir.path()).expect("open");

        let content = b"hello from FILE_FLAG_DELETE_ON_CLOSE";
        wtf.file_mut().write_all(content).expect("write");

        let dest = dir.path().join("committed.txt");
        wtf.commit_to(&dest).expect("commit");

        assert!(dest.exists());
        assert_eq!(fs::read(&dest).expect("read"), content);
    }

    #[test]
    fn large_write_integrity() {
        let dir = tempdir().expect("tempdir");

        // 2 MB of patterned data.
        let size = 2 * 1024 * 1024;
        let pattern: Vec<u8> = (0..=255u8).cycle().take(size).collect();

        let mut wtf = WindowsTempFile::open(dir.path()).expect("open");
        wtf.file_mut().write_all(&pattern).expect("write");

        let dest = dir.path().join("large.bin");
        wtf.commit_to(&dest).expect("commit");

        let data = fs::read(&dest).expect("read");
        assert_eq!(data.len(), size);
        assert_eq!(data, pattern);
    }

    #[test]
    fn drop_without_commit_leaves_no_orphan() {
        let dir = tempdir().expect("tempdir");

        let path;
        {
            let mut wtf = WindowsTempFile::open(dir.path()).expect("open");
            wtf.file_mut()
                .write_all(b"will be orphaned")
                .expect("write");
            path = wtf.temp_path().to_path_buf();
        }

        assert!(
            !path.exists(),
            "no orphaned files after drop without commit"
        );
    }

    #[test]
    fn commit_replaces_existing_file() {
        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("existing.txt");
        fs::write(&dest, b"old content").expect("create existing");

        let mut wtf = WindowsTempFile::open(dir.path()).expect("open");
        wtf.file_mut().write_all(b"new content").expect("write");
        wtf.commit_to(&dest).expect("commit");

        assert_eq!(fs::read_to_string(&dest).expect("read"), "new content");
    }

    #[test]
    fn multiple_temp_files_in_same_directory() {
        let dir = tempdir().expect("tempdir");

        let mut files = Vec::new();
        for i in 0..5 {
            let mut wtf = WindowsTempFile::open(dir.path()).expect("open");
            wtf.file_mut()
                .write_all(format!("file_{i}").as_bytes())
                .expect("write");
            files.push(wtf);
        }

        for (i, wtf) in files.into_iter().enumerate() {
            let dest = dir.path().join(format!("file_{i}.txt"));
            wtf.commit_to(&dest).expect("commit");
        }

        for i in 0..5 {
            let dest = dir.path().join(format!("file_{i}.txt"));
            let content = fs::read_to_string(&dest).expect("read");
            assert_eq!(content, format!("file_{i}"));
        }
    }

    #[test]
    fn temp_file_strategy_create_and_commit() {
        use fast_io::temp_file_strategy::{
            TempFileKind, TempFileStrategy, WindowsTempFileStrategy,
        };

        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("strategy.txt");
        let strategy = WindowsTempFileStrategy;

        let mut handle = strategy.create(&dest).expect("create");
        assert!(matches!(handle.kind, TempFileKind::DeleteOnClose { .. }));
        handle.file.write_all(b"strategy data").expect("write");
        strategy.commit(handle, &dest).expect("commit");

        assert!(dest.exists());
        assert_eq!(fs::read_to_string(&dest).expect("read"), "strategy data");
    }

    #[test]
    fn temp_file_strategy_discard_auto_cleans() {
        use fast_io::temp_file_strategy::{
            TempFileKind, TempFileStrategy, WindowsTempFileStrategy,
        };

        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("strategy_discard.txt");
        let strategy = WindowsTempFileStrategy;

        let handle = strategy.create(&dest).expect("create");
        let temp_path = match &handle.kind {
            TempFileKind::DeleteOnClose { temp_path } => temp_path.clone(),
            _ => panic!("expected delete-on-close"),
        };

        assert!(temp_path.exists());
        strategy.discard(handle);
        assert!(!temp_path.exists());
        assert!(!dest.exists());
    }

    #[test]
    fn default_strategy_uses_delete_on_close() {
        use fast_io::temp_file_strategy::{
            DefaultTempFileStrategy, TempFileKind, TempFileStrategy,
        };

        let dir = tempdir().expect("tempdir");
        let dest = dir.path().join("default.txt");
        let strategy = DefaultTempFileStrategy::default();

        let handle = strategy.create(&dest).expect("create");
        assert!(
            matches!(handle.kind, TempFileKind::DeleteOnClose { .. }),
            "DefaultTempFileStrategy should prefer delete-on-close on Windows"
        );
        strategy.discard(handle);
    }
}

// ---------------------------------------------------------------------------
// Non-Windows fallback tests
// ---------------------------------------------------------------------------

#[cfg(not(target_os = "windows"))]
mod non_windows {
    use super::*;

    #[test]
    fn probe_always_unavailable() {
        let dir = tempdir().expect("tempdir");
        assert_eq!(
            win_tmpfile_probe(dir.path()),
            WinDeleteOnCloseSupport::Unavailable
        );
    }

    #[test]
    fn open_returns_unsupported() {
        let dir = tempdir().expect("tempdir");
        match WindowsTempFile::open(dir.path()) {
            Err(err) => assert_eq!(err.kind(), std::io::ErrorKind::Unsupported),
            Ok(_) => panic!("should fail on non-Windows"),
        }
    }

    #[test]
    fn open_win_temp_file_returns_unavailable() {
        let dir = tempdir().expect("tempdir");
        assert!(matches!(
            open_win_temp_file(dir.path()),
            WinTempFileResult::Unavailable
        ));
    }
}
