use super::*;
#[cfg(unix)]
use std::ffi::OsString;
use std::fs;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tempfile::TempDir;

fn create_test_tree() -> TempDir {
    let dir = TempDir::new().unwrap();
    File::create(dir.path().join("file1.txt")).unwrap();
    File::create(dir.path().join("file2.txt")).unwrap();
    File::create(dir.path().join("file3.txt")).unwrap();
    fs::create_dir(dir.path().join("subdir")).unwrap();
    File::create(dir.path().join("subdir/nested.txt")).unwrap();
    dir
}

#[test]
fn test_cache_new() {
    let cache = BatchedStatCache::new();
    assert!(cache.is_empty());
}

#[test]
fn test_cache_insert_and_get() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    let metadata = fs::metadata(&path).unwrap();
    cache.insert(path.clone(), metadata);

    assert!(cache.get(&path).is_some());
    assert_eq!(cache.len(), 1);
}

#[test]
fn test_get_or_fetch_caches_result() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    let result1 = cache.get_or_fetch(&path, false);
    assert!(result1.is_ok());
    assert_eq!(cache.len(), 1);

    let result2 = cache.get_or_fetch(&path, false);
    assert!(result2.is_ok());
    assert_eq!(cache.len(), 1);

    assert!(Arc::ptr_eq(&result1.unwrap(), &result2.unwrap()));
}

#[test]
fn test_clear() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    cache.get_or_fetch(&path, false).unwrap();
    assert_eq!(cache.len(), 1);

    cache.clear();
    assert!(cache.is_empty());
}

#[cfg(feature = "parallel")]
#[test]
fn test_stat_batch() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();

    let paths: Vec<_> = vec![
        temp.path().join("file1.txt"),
        temp.path().join("file2.txt"),
        temp.path().join("file3.txt"),
    ];

    let path_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
    let results = cache.stat_batch(&path_refs, false);

    assert_eq!(results.len(), 3);
    for result in &results {
        assert!(result.is_ok());
    }

    assert_eq!(cache.len(), 3);
}

#[cfg(feature = "parallel")]
#[test]
fn test_stat_batch_with_errors() {
    let cache = BatchedStatCache::new();

    let paths: Vec<PathBuf> = vec![
        PathBuf::from("/nonexistent1"),
        PathBuf::from("/nonexistent2"),
    ];

    let path_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
    let results = cache.stat_batch(&path_refs, false);

    assert_eq!(results.len(), 2);
    for result in &results {
        assert!(result.is_err());
    }
}

#[test]
fn test_cache_clone_shares_data() {
    let cache1 = BatchedStatCache::new();
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    cache1.get_or_fetch(&path, false).unwrap();
    assert_eq!(cache1.len(), 1);

    let cache2 = cache1.clone();
    assert_eq!(cache2.len(), 1);

    assert!(cache2.get(&path).is_some());
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch() {
    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let name = OsString::from("file1.txt");
    let result = batch.stat_relative(&name, false);
    assert!(result.is_ok());
}

#[cfg(all(unix, feature = "parallel"))]
#[test]
fn test_directory_stat_batch_multiple() {
    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let names = vec![
        OsString::from("file1.txt"),
        OsString::from("file2.txt"),
        OsString::from("file3.txt"),
    ];

    let results = batch.stat_batch_relative(&names, false);
    assert_eq!(results.len(), 3);

    for result in &results {
        assert!(result.is_ok());
    }
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_basic() {
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    if has_statx_support() {
        let result = statx(&path, false);
        assert!(result.is_ok());

        let sr = result.unwrap();
        assert!(sr.is_file());
        assert!(!sr.is_dir());
        assert!(!sr.is_symlink());
        assert_eq!(sr.size, 0);
    }
}

#[test]
fn test_cache_with_capacity() {
    let cache = BatchedStatCache::with_capacity(50);
    assert!(cache.is_empty());
    assert_eq!(cache.len(), 0);
}

#[cfg(feature = "parallel")]
#[test]
fn test_stat_batch_parallel_performance() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();

    for i in 10..50 {
        File::create(temp.path().join(format!("file{i}.txt"))).unwrap();
    }

    let paths: Vec<_> = (0..50)
        .map(|i| temp.path().join(format!("file{i}.txt")))
        .collect();

    let path_refs: Vec<&Path> = paths
        .iter()
        .filter(|p| p.exists())
        .map(|p| p.as_path())
        .collect();

    let results = cache.stat_batch(&path_refs, false);

    for result in &results {
        assert!(result.is_ok());
    }
}

#[test]
fn test_get_returns_none_for_missing() {
    let cache = BatchedStatCache::new();
    let path = PathBuf::from("/this/does/not/exist");

    assert!(cache.get(&path).is_none());
}

#[test]
fn test_insert_and_get_same_path() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    let metadata = fs::metadata(&path).unwrap();
    cache.insert(path.clone(), metadata);

    let retrieved = cache.get(&path);
    assert!(retrieved.is_some());

    let metadata2 = fs::metadata(&path).unwrap();
    cache.insert(path.clone(), metadata2);
    let retrieved2 = cache.get(&path);

    assert_eq!(cache.len(), 1);
    assert!(retrieved2.is_some());
}

#[test]
fn test_get_or_fetch_error_not_cached() {
    let cache = BatchedStatCache::new();
    let nonexistent = PathBuf::from("/definitely/does/not/exist/12345");

    let result1 = cache.get_or_fetch(&nonexistent, false);
    assert!(result1.is_err());

    assert_eq!(cache.len(), 0);
}

#[test]
fn test_follow_symlinks_option() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();

    #[cfg(unix)]
    {
        let target = temp.path().join("file1.txt");
        let link = temp.path().join("link.txt");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        let result_nofollow = cache.get_or_fetch(&link, false);
        assert!(result_nofollow.is_ok());

        cache.clear();
        let result_follow = cache.get_or_fetch(&link, true);
        assert!(result_follow.is_ok());

        assert!(cache.get(&link).is_some());
    }
}

#[cfg(feature = "parallel")]
#[test]
fn test_stat_batch_mixed_results() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();

    let paths: Vec<PathBuf> = vec![
        temp.path().join("file1.txt"),
        PathBuf::from("/nonexistent1"),
        temp.path().join("file2.txt"),
        PathBuf::from("/nonexistent2"),
        temp.path().join("file3.txt"),
    ];

    let path_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();
    let results = cache.stat_batch(&path_refs, false);

    assert_eq!(results.len(), 5);
    assert!(results[0].is_ok());
    assert!(results[1].is_err());
    assert!(results[2].is_ok());
    assert!(results[3].is_err());
    assert!(results[4].is_ok());

    assert_eq!(cache.len(), 3);
}

#[test]
fn test_cache_clone_independence() {
    let cache1 = BatchedStatCache::new();
    let temp = create_test_tree();
    let path1 = temp.path().join("file1.txt");
    let path2 = temp.path().join("file2.txt");

    cache1.get_or_fetch(&path1, false).unwrap();
    assert_eq!(cache1.len(), 1);

    let cache2 = cache1.clone();

    assert_eq!(cache2.len(), 1);
    assert!(cache2.get(&path1).is_some());

    // Adding to one affects the other (shared Arc)
    cache2.get_or_fetch(&path2, false).unwrap();
    assert_eq!(cache1.len(), 2);
    assert_eq!(cache2.len(), 2);
}

#[test]
fn test_clear_resets_length() {
    let cache = BatchedStatCache::new();
    let temp = create_test_tree();

    for i in 1..=3 {
        let path = temp.path().join(format!("file{i}.txt"));
        cache.get_or_fetch(&path, false).unwrap();
    }

    assert_eq!(cache.len(), 3);
    cache.clear();
    assert_eq!(cache.len(), 0);
    assert!(cache.is_empty());
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_nonexistent() {
    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let name = OsString::from("nonexistent.txt");
    let result = batch.stat_relative(&name, false);
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_symlink() {
    let temp = create_test_tree();
    let target = temp.path().join("file1.txt");
    let link = temp.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let name = OsString::from("link.txt");
    let result_nofollow = batch.stat_relative(&name, false);
    assert!(result_nofollow.is_ok());

    let result_follow = batch.stat_relative(&name, true);
    assert!(result_follow.is_ok());
}

#[cfg(all(unix, feature = "parallel"))]
#[test]
fn test_directory_stat_batch_parallel() {
    let temp = create_test_tree();

    for i in 10..30 {
        File::create(temp.path().join(format!("file{i}.txt"))).unwrap();
    }

    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let names: Vec<OsString> = (1..30)
        .map(|i| OsString::from(format!("file{i}.txt")))
        .collect();

    let results = batch.stat_batch_relative(&names, false);

    let success_count = results.iter().filter(|r| r.is_ok()).count();
    assert!(success_count >= 3);
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_empty_names() {
    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let names: Vec<OsString> = vec![];
    let results = batch.stat_batch_relative(&names, false);
    assert_eq!(results.len(), 0);
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_invalid_filename() {
    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();

    let name = OsString::from("file\0name.txt");
    let result = batch.stat_relative(&name, false);
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_open_nonexistent() {
    let result = DirectoryStatBatch::open("/this/directory/does/not/exist");
    assert!(result.is_err());
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_subdirectory() {
    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path().join("subdir")).unwrap();

    let name = OsString::from("nested.txt");
    let result = batch.stat_relative(&name, false);
    assert!(result.is_ok());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_follow_symlinks() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let target = temp.path().join("file1.txt");
    let link = temp.path().join("link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let result_nofollow = statx(&link, false).unwrap();
    assert!(result_nofollow.is_symlink());
    assert!(!result_nofollow.is_file());

    let result_follow = statx(&link, true).unwrap();
    assert!(result_follow.is_file());
    assert!(!result_follow.is_symlink());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_directory() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let result = statx(temp.path().join("subdir"), false).unwrap();
    assert!(result.is_dir());
    assert!(!result.is_file());
    assert!(!result.is_symlink());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_file_with_content() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("sized.txt");
    fs::write(&path, b"hello world").unwrap();

    let result = statx(&path, false).unwrap();
    assert!(result.is_file());
    assert_eq!(result.size, 11);
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_metadata_matches_std() {
    use std::os::unix::fs::MetadataExt;

    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    let sr = statx(&path, false).unwrap();
    let std_meta = fs::symlink_metadata(&path).unwrap();

    assert_eq!(sr.size, std_meta.len());
    assert_eq!(sr.uid, std_meta.uid());
    assert_eq!(sr.gid, std_meta.gid());
    assert_eq!(sr.ino, std_meta.ino());
    assert_eq!(sr.nlink as u64, std_meta.nlink());
    assert_eq!(sr.mode, std_meta.mode() & 0o777_7777);
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_mtime_only() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    let (mtime_sec, _mtime_nsec) = statx_mtime(&path, false).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(mtime_sec > now - 3600, "mtime too old: {mtime_sec}");
    assert!(mtime_sec <= now + 1, "mtime in the future: {mtime_sec}");
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_size_and_mtime() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("combo.txt");
    fs::write(&path, b"1234567890").unwrap();

    let (size, mtime_sec, _mtime_nsec) = statx_size_and_mtime(&path, false).unwrap();
    assert_eq!(size, 10);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;
    assert!(mtime_sec > now - 3600);
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_permissions() {
    use std::os::unix::fs::PermissionsExt;

    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("perms.txt");
    fs::write(&path, b"test").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

    let sr = statx(&path, false).unwrap();
    assert_eq!(sr.permissions(), 0o755);
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_invalid_path() {
    if !has_statx_support() {
        return;
    }

    let result = statx("/invalid\0path", false);
    assert!(result.is_err());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_nonexistent() {
    if has_statx_support() {
        let result = statx("/nonexistent/path/xyz", false);
        assert!(result.is_err());
    }
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_relative_via_directory_batch() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("reltest.txt");
    fs::write(&path, b"relative test").unwrap();

    let batch = DirectoryStatBatch::open(temp.path()).unwrap();
    let name = OsString::from("reltest.txt");
    let sr = batch.statx_relative(&name, false).unwrap();

    assert!(sr.is_file());
    assert_eq!(sr.size, 13);
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_relative_directory() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();
    let name = OsString::from("subdir");
    let sr = batch.statx_relative(&name, false).unwrap();

    assert!(sr.is_dir());
    assert!(!sr.is_file());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_relative_symlink() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let target = temp.path().join("file1.txt");
    let link = temp.path().join("statxlink.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let batch = DirectoryStatBatch::open(temp.path()).unwrap();
    let name = OsString::from("statxlink.txt");

    let sr_nofollow = batch.statx_relative(&name, false).unwrap();
    assert!(sr_nofollow.is_symlink());

    let sr_follow = batch.statx_relative(&name, true).unwrap();
    assert!(sr_follow.is_file());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_relative_nonexistent() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let batch = DirectoryStatBatch::open(temp.path()).unwrap();
    let name = OsString::from("does_not_exist.txt");
    let result = batch.statx_relative(&name, false);
    assert!(result.is_err());
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_result_clone() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");
    let sr = statx(&path, false).unwrap();
    let sr_clone = sr.clone();

    assert_eq!(sr.mode, sr_clone.mode);
    assert_eq!(sr.size, sr_clone.size);
    assert_eq!(sr.uid, sr_clone.uid);
    assert_eq!(sr.ino, sr_clone.ino);
}

#[cfg(all(target_os = "linux", not(target_env = "musl")))]
#[test]
fn test_statx_result_debug() {
    if !has_statx_support() {
        return;
    }

    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");
    let sr = statx(&path, false).unwrap();
    let debug = format!("{sr:?}");
    assert!(debug.contains("StatxResult"));
    assert!(debug.contains("mode"));
    assert!(debug.contains("size"));
}

#[test]
fn test_has_statx_support_does_not_panic() {
    let _ = has_statx_support();
}

/// Verifies that `has_statx_support()` returns consistent results across calls
/// (tests the caching mechanism).
#[test]
fn test_has_statx_support_consistent() {
    let result1 = has_statx_support();
    let result2 = has_statx_support();
    let result3 = has_statx_support();
    assert_eq!(result1, result2);
    assert_eq!(result2, result3);
}

#[cfg(any(not(target_os = "linux"), target_env = "musl"))]
#[test]
fn test_statx_not_supported_on_non_linux() {
    assert!(!has_statx_support());
}

/// Tests that the fallback path (regular stat via `fs::metadata`) still works
/// even on platforms that support statx.
#[test]
fn test_fallback_stat_works() {
    let temp = create_test_tree();
    let path = temp.path().join("file1.txt");

    let metadata = fs::symlink_metadata(&path);
    assert!(metadata.is_ok());
    assert!(metadata.unwrap().is_file());
}

/// Tests that the fallback for directories works on all platforms.
#[test]
fn test_fallback_stat_directory() {
    let temp = create_test_tree();
    let path = temp.path().join("subdir");

    let metadata = fs::symlink_metadata(&path);
    assert!(metadata.is_ok());
    assert!(metadata.unwrap().is_dir());
}

/// Tests that the fallback for symlinks works on all unix platforms.
#[cfg(unix)]
#[test]
fn test_fallback_stat_symlink() {
    let temp = create_test_tree();
    let target = temp.path().join("file1.txt");
    let link = temp.path().join("fallback_link.txt");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    let metadata = fs::symlink_metadata(&link);
    assert!(metadata.is_ok());
    assert!(metadata.unwrap().file_type().is_symlink());
}

#[test]
fn test_cache_thread_safety() {
    use std::sync::Arc;
    use std::thread;

    let temp = create_test_tree();
    let cache = Arc::new(BatchedStatCache::new());
    let path = Arc::new(temp.path().join("file1.txt"));

    let mut handles = vec![];

    for _ in 0..4 {
        let cache_clone = Arc::clone(&cache);
        let path_clone = Arc::clone(&path);

        let handle = thread::spawn(move || {
            for _ in 0..10 {
                let _ = cache_clone.get_or_fetch(&path_clone, false);
            }
        });

        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    assert_eq!(cache.len(), 1);
}

#[test]
fn test_cache_unicode_paths() {
    let temp = create_test_tree();
    let cache = BatchedStatCache::new();

    let unicode_names = vec!["файл.txt", "文件.txt", "ファイル.txt"];

    for name in &unicode_names {
        let path = temp.path().join(name);
        fs::write(&path, b"content").unwrap();
        let result = cache.get_or_fetch(&path, false);
        assert!(result.is_ok());
    }

    assert_eq!(cache.len(), unicode_names.len());
}

#[test]
fn test_cache_paths_with_spaces() {
    let temp = create_test_tree();
    let cache = BatchedStatCache::new();

    let path = temp.path().join("file with spaces.txt");
    fs::write(&path, b"content").unwrap();

    let result = cache.get_or_fetch(&path, false);
    assert!(result.is_ok());
    assert_eq!(cache.len(), 1);
}

#[cfg(feature = "parallel")]
#[test]
fn test_stat_batch_empty_slice() {
    let cache = BatchedStatCache::new();
    let paths: Vec<&Path> = vec![];

    let results = cache.stat_batch(&paths, false);
    assert_eq!(results.len(), 0);
}

#[test]
fn test_cache_stress_test() {
    let temp = create_test_tree();
    let cache = BatchedStatCache::with_capacity(1000);

    let paths: Vec<_> = (0..100)
        .map(|i| {
            let path = temp.path().join(format!("stress{i}.txt"));
            fs::write(&path, format!("content{i}")).unwrap();
            path
        })
        .collect();

    for _ in 0..3 {
        for path in &paths {
            let result = cache.get_or_fetch(path, false);
            assert!(result.is_ok());
        }
    }

    assert_eq!(cache.len(), 100);
}

#[cfg(unix)]
#[test]
fn test_directory_stat_batch_special_characters() {
    let temp = create_test_tree();

    let special_name = "file-with-dash.txt";
    File::create(temp.path().join(special_name)).unwrap();

    let batch = DirectoryStatBatch::open(temp.path()).unwrap();
    let name = OsString::from(special_name);
    let result = batch.stat_relative(&name, false);
    assert!(result.is_ok());
}

#[test]
fn test_get_or_fetch_consistency() {
    let temp = create_test_tree();
    let cache = BatchedStatCache::new();
    let path = temp.path().join("file1.txt");

    let result1 = cache.get_or_fetch(&path, false).unwrap();
    let result2 = cache.get_or_fetch(&path, false).unwrap();
    let result3 = cache.get(&path).unwrap();

    assert!(Arc::ptr_eq(&result1, &result2));
    assert!(Arc::ptr_eq(&result2, &result3));
}
