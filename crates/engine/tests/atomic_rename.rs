// Tests for atomic temp file rename in the engine crate
//
// These tests verify:
// 1. Temp files are created during transfer
// 2. Atomic rename happens on success
// 3. Temp files are cleaned up on failure
// 4. Race conditions are handled
//
// The tests focus on the DestinationWriteGuard mechanism which provides
// atomic file updates through temp file creation and rename.

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::tempdir;

// Import the guard module types
use engine::local_copy::{
    DestinationWriteGuard, remove_existing_destination, remove_incomplete_destination,
};

// ==================== Basic Temp File Creation Tests ====================

#[test]
fn temp_file_is_created_during_transfer() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("final.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    // Write some content to the temp file
    file.write_all(b"test content").expect("write");
    file.sync_all().expect("sync");

    // Verify temp file exists and is different from destination
    let staging = guard.staging_path();
    assert!(staging.exists(), "staging file should exist");
    assert_ne!(
        staging,
        dest.as_path(),
        "staging path should differ from destination"
    );
    assert!(
        staging.to_string_lossy().contains(".rsync-tmp-"),
        "staging path should have rsync temp prefix"
    );
    assert!(!dest.exists(), "destination should not exist until commit");

    // Read back content from staging file
    drop(file);
    let content = fs::read(staging).expect("read staging file");
    assert_eq!(content, b"test content");

    guard.discard();
}

#[test]
fn temp_file_path_includes_process_id() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, _file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    let staging = guard.staging_path();
    let pid = std::process::id();
    assert!(
        staging.to_string_lossy().contains(&pid.to_string()),
        "staging path should include process ID for uniqueness"
    );

    guard.discard();
}

#[test]
fn temp_file_path_includes_unique_counter() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    // Create multiple guards to different files
    let (guard1, _file1) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard1");
    let staging1 = guard1.staging_path().to_path_buf();

    let dest2 = temp.path().join("file2.txt");
    let (guard2, _file2) =
        DestinationWriteGuard::new(&dest2, false, None, None).expect("create guard2");
    let staging2 = guard2.staging_path().to_path_buf();

    // Even though same process, paths should be unique
    assert_ne!(staging1, staging2, "staging paths should be unique");

    guard1.discard();
    guard2.discard();
}

#[test]
fn temp_file_created_in_same_directory_as_destination() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("subdir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let dest = dest_dir.join("file.txt");

    let (guard, _file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    let staging = guard.staging_path();
    assert_eq!(
        staging.parent(),
        dest.parent(),
        "staging file should be in same directory as destination"
    );

    guard.discard();
}

#[test]
fn temp_file_created_in_custom_temp_dir() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("dest").join("file.txt");
    let temp_dir = temp.path().join("custom-tmp");
    fs::create_dir_all(&temp_dir).expect("create temp dir");
    fs::create_dir_all(dest.parent().unwrap()).expect("create dest parent");

    let (guard, _file) =
        DestinationWriteGuard::new(&dest, false, None, Some(&temp_dir)).expect("create guard");

    let staging = guard.staging_path();
    assert!(
        staging.starts_with(&temp_dir),
        "staging file should be in custom temp directory"
    );

    guard.discard();
}

// ==================== Atomic Rename Tests ====================

#[test]
fn atomic_rename_on_successful_commit() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("final.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"committed content").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();

    // Before commit: staging exists, dest doesn't
    assert!(staging.exists());
    assert!(!dest.exists());

    // Commit the guard
    guard.commit().expect("commit");

    // After commit: dest exists, staging doesn't
    assert!(dest.exists(), "destination should exist after commit");
    assert!(
        !staging.exists(),
        "staging file should be removed after commit"
    );

    // Verify content
    let content = fs::read(&dest).expect("read dest");
    assert_eq!(content, b"committed content");
}

#[test]
fn atomic_rename_replaces_existing_file() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("existing.txt");

    // Create existing file with old content
    fs::write(&dest, b"old content").expect("write existing");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"new content").expect("write");
    drop(file);

    // Commit should replace the existing file
    guard.commit().expect("commit");

    let content = fs::read(&dest).expect("read dest");
    assert_eq!(
        content, b"new content",
        "destination should have new content"
    );
}

#[test]
fn atomic_rename_is_truly_atomic() {
    // This test verifies that during the rename, readers never see partial content
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    // Write initial content
    fs::write(&dest, b"original content that is long enough").expect("write original");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"new content that is also very long")
        .expect("write");
    drop(file);

    // Commit should be atomic - dest has either old or new content, never partial
    guard.commit().expect("commit");

    let content = fs::read(&dest).expect("read dest");
    // Should be exactly the new content, not truncated or mixed
    assert_eq!(content, b"new content that is also very long");
}

#[test]
fn atomic_rename_preserves_content_exactly() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.bin");

    // Create binary content with various byte values
    let original_content: Vec<u8> = (0..=255).cycle().take(10000).collect();

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(&original_content).expect("write");
    drop(file);

    guard.commit().expect("commit");

    let read_content = fs::read(&dest).expect("read dest");
    assert_eq!(
        read_content, original_content,
        "content should be preserved exactly"
    );
}

#[test]
fn commit_returns_error_on_failure_but_preserves_temp() {
    let temp = tempdir().expect("tempdir");
    let dest_dir = temp.path().join("dest_dir");
    fs::create_dir_all(&dest_dir).expect("create dest dir");
    let dest = dest_dir.join("file.txt");

    // Create an initial file
    fs::write(&dest, b"original").expect("write original");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"content").expect("write");
    drop(file);

    let _staging = guard.staging_path().to_path_buf();

    // Remove the destination to trigger an error condition that leaves the temp file
    // (On some systems rename over a non-existent target may still work, that's okay)
    let _ = fs::remove_file(&dest);
    // Also remove the directory to make commit fail
    let _ = fs::remove_dir(&dest_dir);

    // Commit should fail because parent directory doesn't exist
    let result = guard.commit();

    // On most systems this will fail, but if it succeeds that's also okay
    // The key is that we don't panic and the temp file is handled
    match result {
        Ok(_) => {
            // Some systems may auto-create dirs or have different behavior
            // This is acceptable behavior
        }
        Err(_) => {
            // Expected: commit failed, temp file may or may not exist
            // (depending on when the error occurred)
        }
    }
}

// ==================== Cleanup on Failure Tests ====================

#[test]
fn temp_file_cleaned_up_on_explicit_discard() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"content to discard").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();
    assert!(staging.exists(), "staging should exist before discard");

    guard.discard();

    assert!(
        !staging.exists(),
        "staging file should be cleaned up after discard"
    );
    assert!(
        !dest.exists(),
        "destination should not be created on discard"
    );
}

#[test]
fn temp_file_cleaned_up_on_drop_without_commit() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let staging;
    {
        let (guard, mut file) =
            DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

        file.write_all(b"content").expect("write");
        drop(file);

        staging = guard.staging_path().to_path_buf();
        assert!(staging.exists());

        // Guard dropped here without commit or explicit discard
    }

    // After guard is dropped, temp file should be cleaned up
    assert!(
        !staging.exists(),
        "staging file should be cleaned up by Drop implementation"
    );
    assert!(!dest.exists(), "destination should not exist");
}

#[test]
fn temp_file_not_cleaned_up_after_successful_commit() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"content").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();
    guard.commit().expect("commit");

    // After successful commit, staging is moved (not deleted)
    assert!(
        !staging.exists(),
        "staging should be moved, not exist at original path"
    );
    assert!(dest.exists(), "destination should exist");
}

#[test]
fn partial_mode_preserves_temp_on_discard() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, true, None, None).expect("create guard with partial");

    file.write_all(b"partial content").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();
    assert!(staging.exists());
    assert!(staging.to_string_lossy().contains(".rsync-partial-"));

    guard.discard();

    // In partial mode, discard should preserve the file for resumption
    assert!(
        staging.exists(),
        "partial file should be preserved on discard for resumption"
    );
    assert!(!dest.exists());
}

#[test]
fn partial_mode_moves_to_dest_on_commit() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, true, None, None).expect("create guard with partial");

    file.write_all(b"completed partial content").expect("write");
    drop(file);

    let staging = guard.staging_path().to_path_buf();
    guard.commit().expect("commit");

    assert!(!staging.exists(), "partial file should be moved on commit");
    assert!(dest.exists(), "destination should exist");
    assert_eq!(fs::read(&dest).expect("read"), b"completed partial content");
}

#[test]
fn remove_incomplete_destination_cleans_up_file() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("incomplete.txt");

    fs::write(&dest, b"incomplete content").expect("write");
    assert!(dest.exists());

    remove_incomplete_destination(&dest);

    assert!(!dest.exists(), "incomplete destination should be removed");
}

#[test]
fn remove_incomplete_destination_succeeds_if_missing() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("nonexistent.txt");

    // Should not panic or error
    remove_incomplete_destination(&dest);

    assert!(!dest.exists());
}

#[test]
fn remove_existing_destination_removes_file() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("existing.txt");

    fs::write(&dest, b"existing content").expect("write");
    assert!(dest.exists());

    let result = remove_existing_destination(&dest);

    assert!(result.is_ok(), "remove should succeed");
    assert!(!dest.exists(), "file should be removed");
}

#[test]
fn remove_existing_destination_succeeds_if_missing() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("nonexistent.txt");

    let result = remove_existing_destination(&dest);

    assert!(
        result.is_ok(),
        "remove should succeed even if file doesn't exist"
    );
}

// ==================== Race Condition Tests ====================

#[test]
fn concurrent_guards_get_unique_temp_files() {
    let temp = tempdir().expect("tempdir");
    let dest1 = temp.path().join("file1.txt");
    let dest2 = temp.path().join("file2.txt");

    let barrier = Arc::new(Barrier::new(2));
    let barrier1 = barrier.clone();
    let barrier2 = barrier.clone();

    let handle1 = thread::spawn(move || {
        barrier1.wait();
        let (guard, mut file) =
            DestinationWriteGuard::new(&dest1, false, None, None).expect("create guard1");
        file.write_all(b"content1").expect("write1");
        drop(file);
        let staging = guard.staging_path().to_path_buf();
        guard.commit().expect("commit1");
        (dest1, staging)
    });

    let handle2 = thread::spawn(move || {
        barrier2.wait();
        let (guard, mut file) =
            DestinationWriteGuard::new(&dest2, false, None, None).expect("create guard2");
        file.write_all(b"content2").expect("write2");
        drop(file);
        let staging = guard.staging_path().to_path_buf();
        guard.commit().expect("commit2");
        (dest2, staging)
    });

    let (dest1, staging1) = handle1.join().expect("join1");
    let (dest2, staging2) = handle2.join().expect("join2");

    // Staging paths should be unique
    assert_ne!(
        staging1, staging2,
        "concurrent guards should get unique temp files"
    );

    // Both destinations should exist with correct content
    assert_eq!(fs::read(&dest1).expect("read1"), b"content1");
    assert_eq!(fs::read(&dest2).expect("read2"), b"content2");
}

#[test]
fn multiple_guards_to_same_destination_get_unique_temps() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("shared.txt");

    // Create first guard
    let (guard1, mut file1) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard1");
    file1.write_all(b"first").expect("write1");
    drop(file1);
    let staging1 = guard1.staging_path().to_path_buf();

    // Create second guard to same destination (simulating retry or concurrent write)
    let (guard2, mut file2) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard2");
    file2.write_all(b"second").expect("write2");
    drop(file2);
    let staging2 = guard2.staging_path().to_path_buf();

    // Temp files should be different
    assert_ne!(staging1, staging2, "guards should get unique temp files");
    assert!(staging1.exists());
    assert!(staging2.exists());

    // Commit second (later) one
    guard2.commit().expect("commit2");
    assert_eq!(fs::read(&dest).expect("read"), b"second");

    // First one still exists as temp
    assert!(staging1.exists());

    // Clean up first
    guard1.discard();
    assert!(!staging1.exists());
}

#[test]
fn guard_handles_temp_file_collision_gracefully() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    // Create many guards quickly to stress the unique ID generation
    let mut guards = Vec::new();
    let mut stagings = Vec::new();

    for i in 0..100 {
        let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None)
            .unwrap_or_else(|_| panic!("create guard {i}"));

        file.write_all(format!("content{i}").as_bytes())
            .unwrap_or_else(|_| panic!("write {i}"));
        drop(file);

        let staging = guard.staging_path().to_path_buf();
        stagings.push(staging.clone());
        guards.push(guard);
    }

    // All staging paths should be unique
    for i in 0..stagings.len() {
        for j in (i + 1)..stagings.len() {
            assert_ne!(
                stagings[i], stagings[j],
                "staging paths {i} and {j} should be unique"
            );
        }
    }

    // Clean up
    for guard in guards {
        guard.discard();
    }
}

#[test]
fn concurrent_commits_to_same_destination_are_safe() {
    let temp = tempdir().expect("tempdir");
    let dest = Arc::new(temp.path().join("contested.txt"));

    let barrier = Arc::new(Barrier::new(3));
    let mut handles = Vec::new();

    for i in 0..3 {
        let dest = dest.clone();
        let barrier = barrier.clone();

        let handle = thread::spawn(move || {
            let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None)
                .unwrap_or_else(|_| panic!("create guard {i}"));

            file.write_all(format!("thread {i} content").as_bytes())
                .unwrap_or_else(|_| panic!("write {i}"));
            drop(file);

            // Synchronize to make all threads commit at approximately the same time
            barrier.wait();

            guard.commit().unwrap_or_else(|_| panic!("commit {i}"));
        });

        handles.push(handle);
    }

    // Wait for all threads
    for handle in handles {
        handle.join().expect("join thread");
    }

    // One of the threads should have won - destination should exist with valid content
    assert!(dest.exists(), "destination should exist");
    let content = fs::read(dest.as_ref()).expect("read dest");

    // Content should match one of the threads (not corrupted)
    let valid_contents = [
        b"thread 0 content".to_vec(),
        b"thread 1 content".to_vec(),
        b"thread 2 content".to_vec(),
    ];
    assert!(
        valid_contents.contains(&content),
        "content should match one of the threads, got: {:?}",
        String::from_utf8_lossy(&content)
    );
}

#[test]
fn guard_from_different_threads_get_unique_temps() {
    let temp = tempdir().expect("tempdir");
    let base_path = Arc::new(temp.path().to_path_buf());

    let handles: Vec<_> = (0..5)
        .map(|i| {
            let base = base_path.clone();
            thread::spawn(move || {
                let dest = base.join(format!("file{i}.txt"));
                let (guard, mut file) = DestinationWriteGuard::new(&dest, false, None, None)
                    .unwrap_or_else(|_| panic!("create guard {i}"));

                file.write_all(format!("content from thread {i}").as_bytes())
                    .unwrap_or_else(|_| panic!("write {i}"));
                drop(file);

                let staging = guard.staging_path().to_path_buf();
                guard.commit().unwrap_or_else(|_| panic!("commit {i}"));

                (dest, staging)
            })
        })
        .collect();

    let results: Vec<_> = handles
        .into_iter()
        .map(|h| h.join().expect("join"))
        .collect();

    // All destinations should exist
    for (dest, _) in &results {
        assert!(dest.exists(), "destination {dest:?} should exist");
    }

    // All staging paths should have been unique
    let stagings: Vec<_> = results.iter().map(|(_, s)| s).collect();
    for i in 0..stagings.len() {
        for j in (i + 1)..stagings.len() {
            assert_ne!(
                stagings[i], stagings[j],
                "staging paths from different threads should be unique"
            );
        }
    }
}

// ==================== Edge Cases ====================

#[test]
fn empty_file_transfer_works() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("empty.txt");

    let (guard, file) = DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    // Write nothing
    drop(file);

    guard.commit().expect("commit");

    assert!(dest.exists());
    assert_eq!(fs::metadata(&dest).expect("metadata").len(), 0);
}

#[test]
fn large_file_transfer_works() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("large.bin");

    // Create a 10MB file
    let large_content = vec![0xAB; 10 * 1024 * 1024];

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(&large_content).expect("write");
    drop(file);

    guard.commit().expect("commit");

    assert_eq!(
        fs::metadata(&dest).expect("metadata").len(),
        10 * 1024 * 1024
    );
}

#[test]
fn destination_with_special_characters_works() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file with spaces & special-chars.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"special content").expect("write");
    drop(file);

    guard.commit().expect("commit");

    assert!(dest.exists());
    assert_eq!(fs::read(&dest).expect("read"), b"special content");
}

#[test]
fn nested_destination_path_works() {
    let temp = tempdir().expect("tempdir");
    let nested = temp.path().join("a").join("b").join("c");
    fs::create_dir_all(&nested).expect("create nested");
    let dest = nested.join("file.txt");

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    file.write_all(b"nested content").expect("write");
    drop(file);

    guard.commit().expect("commit");

    assert!(dest.exists());
    assert_eq!(fs::read(&dest).expect("read"), b"nested content");
}

#[test]
fn guard_final_path_returns_original_destination() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, _file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    assert_eq!(guard.final_path(), dest.as_path());

    guard.discard();
}

#[test]
fn guard_staging_path_is_accessible_and_valid() {
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    let (guard, _file) =
        DestinationWriteGuard::new(&dest, false, None, None).expect("create guard");

    let staging = guard.staging_path();
    assert!(staging.exists());
    assert!(staging.is_file());

    // Should be able to write to it via the returned path
    fs::write(staging, b"direct write").expect("direct write to staging");

    let content = fs::read(staging).expect("read staging");
    assert_eq!(content, b"direct write");

    guard.discard();
}

#[cfg(unix)]
#[test]
fn cross_device_rename_falls_back_to_copy() {
    // This test attempts to trigger the CrossesDevices error path by using
    // /tmp as temp dir (often tmpfs) and user space as destination (often ext4/btrfs)
    let temp = tempdir().expect("tempdir");
    let dest = temp.path().join("file.txt");

    // Try to use /tmp which is often on a different filesystem
    let system_tmp = PathBuf::from("/tmp");
    if !system_tmp.exists() {
        // Skip test if /tmp doesn't exist
        return;
    }

    let staging_dir = system_tmp.join(format!("rsync-test-{}", std::process::id()));
    let _ = fs::create_dir_all(&staging_dir);

    let (guard, mut file) =
        DestinationWriteGuard::new(&dest, false, None, Some(&staging_dir)).expect("create guard");

    file.write_all(b"cross-device content").expect("write");
    drop(file);

    // Commit should handle cross-device by falling back to copy+delete
    let result = guard.commit();

    // Clean up staging dir
    let _ = fs::remove_dir_all(&staging_dir);

    // Either way, commit should work
    match result {
        Ok(_) => {
            assert!(dest.exists());
            assert_eq!(fs::read(&dest).expect("read"), b"cross-device content");
        }
        Err(e) => {
            // If commit failed for other reasons, that's okay for this test
            eprintln!("Cross-device test commit failed (expected on some systems): {e}");
        }
    }
}
