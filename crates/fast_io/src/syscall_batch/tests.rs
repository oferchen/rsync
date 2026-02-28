//! Tests for batched metadata operations.

use super::*;
use std::fs::File;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::SystemTime;
use tempfile::TempDir;

/// Helper to create a temp file with content.
fn create_test_file(dir: &TempDir, name: &str, content: &[u8]) -> io::Result<PathBuf> {
    let path = dir.path().join(name);
    let mut file = File::create(&path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(path)
}

#[test]
fn test_individual_stat() {
    let temp_dir = TempDir::new().unwrap();
    let path = create_test_file(&temp_dir, "test.txt", b"hello").unwrap();

    let ops = vec![MetadataOp::Stat(path.clone())];
    let results = execute_metadata_ops_individual(&ops);

    assert_eq!(results.len(), 1);
    match &results[0] {
        MetadataResult::Stat(Ok(metadata)) => {
            assert_eq!(metadata.len(), 5);
        }
        _ => panic!("Expected successful Stat result"),
    }
}

#[test]
fn test_individual_lstat() {
    let temp_dir = TempDir::new().unwrap();
    let path = create_test_file(&temp_dir, "test.txt", b"hello world").unwrap();

    let ops = vec![MetadataOp::Lstat(path.clone())];
    let results = execute_metadata_ops_individual(&ops);

    assert_eq!(results.len(), 1);
    match &results[0] {
        MetadataResult::Stat(Ok(metadata)) => {
            assert_eq!(metadata.len(), 11);
        }
        _ => panic!("Expected successful Stat result"),
    }
}

#[test]
fn test_batched_stat_multiple() {
    let temp_dir = TempDir::new().unwrap();
    let path1 = create_test_file(&temp_dir, "file1.txt", b"content1").unwrap();
    let path2 = create_test_file(&temp_dir, "file2.txt", b"content22").unwrap();
    let path3 = create_test_file(&temp_dir, "file3.txt", b"content333").unwrap();

    let ops = vec![
        MetadataOp::Stat(path1.clone()),
        MetadataOp::Stat(path2.clone()),
        MetadataOp::Stat(path3.clone()),
    ];
    let results = execute_metadata_ops_batched(&ops);

    assert_eq!(results.len(), 3);
    match &results[0] {
        MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 8),
        _ => panic!("Expected successful Stat result"),
    }
    match &results[1] {
        MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 9),
        _ => panic!("Expected successful Stat result"),
    }
    match &results[2] {
        MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 10),
        _ => panic!("Expected successful Stat result"),
    }
}

#[cfg(unix)]
#[test]
fn test_set_times() {
    let temp_dir = TempDir::new().unwrap();
    let path = create_test_file(&temp_dir, "test.txt", b"hello").unwrap();

    let new_mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1000000);
    let ops = vec![MetadataOp::SetTimes {
        path: path.clone(),
        atime: None,
        mtime: Some(new_mtime),
    }];
    let results = execute_metadata_ops_individual(&ops);

    assert_eq!(results.len(), 1);
    match &results[0] {
        MetadataResult::SetTimes(Ok(())) => {
            // Verify the time was set
            let metadata = std::fs::metadata(&path).unwrap();
            let mtime = metadata.modified().unwrap();
            // Allow small delta due to filesystem time granularity
            let delta = if mtime > new_mtime {
                mtime.duration_since(new_mtime).unwrap()
            } else {
                new_mtime.duration_since(mtime).unwrap()
            };
            assert!(delta.as_secs() < 2, "mtime delta too large");
        }
        _ => panic!("Expected successful SetTimes result"),
    }
}

#[cfg(unix)]
#[test]
fn test_set_permissions() {
    let temp_dir = TempDir::new().unwrap();
    let path = create_test_file(&temp_dir, "test.txt", b"hello").unwrap();

    let ops = vec![MetadataOp::SetPermissions {
        path: path.clone(),
        mode: 0o644,
    }];
    let results = execute_metadata_ops_individual(&ops);

    assert_eq!(results.len(), 1);
    match &results[0] {
        MetadataResult::SetPermissions(Ok(())) => {
            // Verify permissions were set
            use std::os::unix::fs::PermissionsExt;
            let metadata = std::fs::metadata(&path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o644);
        }
        _ => panic!("Expected successful SetPermissions result"),
    }
}

#[test]
fn test_threshold_routing() {
    let temp_dir = TempDir::new().unwrap();

    // Create BATCH_THRESHOLD files
    let mut paths = Vec::new();
    for i in 0..BATCH_THRESHOLD {
        let path = create_test_file(&temp_dir, &format!("file{i}.txt"), b"test").unwrap();
        paths.push(path);
    }

    // Below threshold - should use individual path
    let ops_below: Vec<_> = paths
        .iter()
        .take(BATCH_THRESHOLD - 1)
        .map(|p| MetadataOp::Stat(p.clone()))
        .collect();
    let results_below = execute_metadata_ops(&ops_below);
    assert_eq!(results_below.len(), BATCH_THRESHOLD - 1);

    // At threshold - should use batched path
    let ops_at: Vec<_> = paths
        .iter()
        .take(BATCH_THRESHOLD)
        .map(|p| MetadataOp::Stat(p.clone()))
        .collect();
    let results_at = execute_metadata_ops(&ops_at);
    assert_eq!(results_at.len(), BATCH_THRESHOLD);

    // Verify all results are successful
    for result in results_below.iter().chain(results_at.iter()) {
        match result {
            MetadataResult::Stat(Ok(_)) => {}
            _ => panic!("Expected successful Stat result"),
        }
    }
}

#[test]
fn test_batched_preserves_order() {
    let temp_dir = TempDir::new().unwrap();
    let path1 = create_test_file(&temp_dir, "file1.txt", b"a").unwrap();
    let path2 = create_test_file(&temp_dir, "file2.txt", b"bb").unwrap();
    let path3 = create_test_file(&temp_dir, "file3.txt", b"ccc").unwrap();

    // Mix different operation types
    let ops = vec![
        MetadataOp::Lstat(path1.clone()), // index 0
        MetadataOp::Stat(path2.clone()),  // index 1
        MetadataOp::Lstat(path3.clone()), // index 2
    ];
    let results = execute_metadata_ops_batched(&ops);

    assert_eq!(results.len(), 3);
    // Results should be in original order despite grouping
    match &results[0] {
        MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 1),
        _ => panic!("Expected Stat at index 0"),
    }
    match &results[1] {
        MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 2),
        _ => panic!("Expected Stat at index 1"),
    }
    match &results[2] {
        MetadataResult::Stat(Ok(metadata)) => assert_eq!(metadata.len(), 3),
        _ => panic!("Expected Stat at index 2"),
    }
}

#[cfg(unix)]
#[test]
fn test_mixed_operations() {
    let temp_dir = TempDir::new().unwrap();
    let path1 = create_test_file(&temp_dir, "file1.txt", b"test1").unwrap();
    let path2 = create_test_file(&temp_dir, "file2.txt", b"test2").unwrap();
    let path3 = create_test_file(&temp_dir, "file3.txt", b"test3").unwrap();

    let new_mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(2000000);
    let ops = vec![
        MetadataOp::Stat(path1.clone()),
        MetadataOp::SetPermissions {
            path: path2.clone(),
            mode: 0o755,
        },
        MetadataOp::Lstat(path3.clone()),
        MetadataOp::SetTimes {
            path: path1.clone(),
            atime: None,
            mtime: Some(new_mtime),
        },
    ];
    let results = execute_metadata_ops_batched(&ops);

    assert_eq!(results.len(), 4);

    // Verify each result type
    match &results[0] {
        MetadataResult::Stat(Ok(_)) => {}
        _ => panic!("Expected Stat at index 0"),
    }
    match &results[1] {
        MetadataResult::SetPermissions(Ok(())) => {}
        _ => panic!("Expected SetPermissions at index 1"),
    }
    match &results[2] {
        MetadataResult::Stat(Ok(_)) => {}
        _ => panic!("Expected Stat at index 2"),
    }
    match &results[3] {
        MetadataResult::SetTimes(Ok(())) => {}
        _ => panic!("Expected SetTimes at index 3"),
    }
}

#[test]
fn test_nonexistent_file_in_batch() {
    let temp_dir = TempDir::new().unwrap();
    let path1 = create_test_file(&temp_dir, "exists.txt", b"hello").unwrap();
    let path2 = temp_dir.path().join("does_not_exist.txt");
    let path3 = create_test_file(&temp_dir, "exists2.txt", b"world").unwrap();

    let ops = vec![
        MetadataOp::Stat(path1.clone()),
        MetadataOp::Stat(path2.clone()),
        MetadataOp::Stat(path3.clone()),
    ];
    let results = execute_metadata_ops_batched(&ops);

    assert_eq!(results.len(), 3);

    // First should succeed
    match &results[0] {
        MetadataResult::Stat(Ok(metadata)) => {
            assert_eq!(metadata.len(), 5);
        }
        _ => panic!("Expected successful Stat at index 0"),
    }

    // Second should fail
    match &results[1] {
        MetadataResult::Stat(Err(_)) => {}
        _ => panic!("Expected failed Stat at index 1"),
    }

    // Third should succeed despite middle failure
    match &results[2] {
        MetadataResult::Stat(Ok(metadata)) => {
            assert_eq!(metadata.len(), 5);
        }
        _ => panic!("Expected successful Stat at index 2"),
    }
}

#[test]
fn test_empty_batch() {
    let ops: Vec<MetadataOp> = vec![];
    let results = execute_metadata_ops(&ops);
    assert_eq!(results.len(), 0);

    let results_individual = execute_metadata_ops_individual(&ops);
    assert_eq!(results_individual.len(), 0);

    let results_batched = execute_metadata_ops_batched(&ops);
    assert_eq!(results_batched.len(), 0);
}

#[test]
fn test_parity_individual_vs_batched() {
    let temp_dir = TempDir::new().unwrap();
    let path1 = create_test_file(&temp_dir, "file1.txt", b"content1").unwrap();
    let path2 = create_test_file(&temp_dir, "file2.txt", b"content2").unwrap();
    let path3 = temp_dir.path().join("nonexistent.txt");
    let path4 = create_test_file(&temp_dir, "file4.txt", b"content4").unwrap();

    let ops = vec![
        MetadataOp::Stat(path1.clone()),
        MetadataOp::Lstat(path2.clone()),
        MetadataOp::Stat(path3.clone()),
        MetadataOp::Lstat(path4.clone()),
    ];

    let results_individual = execute_metadata_ops_individual(&ops);
    let results_batched = execute_metadata_ops_batched(&ops);

    assert_eq!(results_individual.len(), results_batched.len());

    // Compare results
    for (i, (ind, bat)) in results_individual
        .iter()
        .zip(results_batched.iter())
        .enumerate()
    {
        match (ind, bat) {
            (MetadataResult::Stat(Ok(m1)), MetadataResult::Stat(Ok(m2))) => {
                assert_eq!(m1.len(), m2.len(), "Mismatch at index {i}");
                assert_eq!(m1.is_dir(), m2.is_dir(), "Mismatch at index {i}");
            }
            (MetadataResult::Stat(Err(_)), MetadataResult::Stat(Err(_))) => {
                // Both failed - this is expected for nonexistent file
            }
            _ => panic!("Result type mismatch at index {i}: {ind:?} vs {bat:?}"),
        }
    }
}
