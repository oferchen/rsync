use super::*;
use std::io;
use std::io::Write;
use tempfile::TempDir;

fn setup_source(dir: &std::path::Path, name: &str, content: &[u8]) -> std::path::PathBuf {
    let path = dir.join(name);
    let mut file = std::fs::File::create(&path).expect("create source");
    file.write_all(content).expect("write source");
    path
}

fn setup_test_files(
    dir: &std::path::Path,
    name: &str,
    content: &[u8],
) -> (std::path::PathBuf, std::path::PathBuf) {
    let src = dir.join(format!("{name}_src.txt"));
    let dst = dir.join(format!("{name}_dst.txt"));
    std::fs::write(&src, content).expect("write source file");
    (src, dst)
}

#[test]
fn copy_result_new_and_accessors() {
    let result = CopyResult::new(1024, CopyMethod::StandardCopy);
    assert_eq!(result.bytes_copied, 1024);
    assert_eq!(result.method, CopyMethod::StandardCopy);
    assert!(!result.is_zero_copy());
}

#[test]
fn copy_result_is_zero_copy() {
    assert!(CopyResult::new(0, CopyMethod::Ficlone).is_zero_copy());
    assert!(CopyResult::new(0, CopyMethod::CopyFileRange).is_zero_copy());
    assert!(CopyResult::new(0, CopyMethod::Clonefile).is_zero_copy());
    assert!(CopyResult::new(0, CopyMethod::ReFsReflink).is_zero_copy());
    assert!(!CopyResult::new(0, CopyMethod::Copyfile).is_zero_copy());
    assert!(!CopyResult::new(0, CopyMethod::CopyFileEx).is_zero_copy());
    assert!(!CopyResult::new(0, CopyMethod::StandardCopy).is_zero_copy());
}

#[test]
fn copy_method_display() {
    assert_eq!(CopyMethod::Ficlone.to_string(), "ficlone");
    assert_eq!(CopyMethod::CopyFileRange.to_string(), "copy_file_range");
    assert_eq!(CopyMethod::Clonefile.to_string(), "clonefile");
    assert_eq!(CopyMethod::Copyfile.to_string(), "copyfile");
    assert_eq!(CopyMethod::ReFsReflink.to_string(), "ReFS reflink");
    assert_eq!(CopyMethod::CopyFileEx.to_string(), "CopyFileExW");
    assert_eq!(CopyMethod::StandardCopy.to_string(), "standard copy");
}

#[test]
fn copy_method_equality_and_hash() {
    use std::collections::HashSet;

    let mut set = HashSet::new();
    set.insert(CopyMethod::Ficlone);
    set.insert(CopyMethod::CopyFileRange);
    set.insert(CopyMethod::Clonefile);
    set.insert(CopyMethod::ReFsReflink);
    set.insert(CopyMethod::StandardCopy);
    assert_eq!(set.len(), 5);
    assert!(set.contains(&CopyMethod::Ficlone));
    assert!(set.contains(&CopyMethod::CopyFileRange));
    assert!(set.contains(&CopyMethod::ReFsReflink));
}

#[test]
fn default_platform_copy_small_file() {
    let temp = TempDir::new().expect("create temp dir");
    let content = b"Hello, platform copy!";
    let src = setup_source(temp.path(), "small_src.txt", content);
    let dst = temp.path().join("small_dst.txt");

    let copier = DefaultPlatformCopy::new();
    let result = copier
        .copy_file(&src, &dst, content.len() as u64)
        .expect("copy succeeds");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, content);
    assert!(result.bytes_copied > 0 || result.method == CopyMethod::Clonefile);
}

#[test]
fn default_platform_copy_empty_file() {
    let temp = TempDir::new().expect("create temp dir");
    let src = setup_source(temp.path(), "empty_src.txt", b"");
    let dst = temp.path().join("empty_dst.txt");

    let copier = DefaultPlatformCopy::new();
    let result = copier.copy_file(&src, &dst, 0).expect("copy succeeds");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, b"");
    assert!(
        result.bytes_copied == 0,
        "empty file should copy 0 bytes, got {}",
        result.bytes_copied
    );
}

#[test]
fn default_platform_copy_large_file() {
    let temp = TempDir::new().expect("create temp dir");
    let size = 256 * 1024; // 256KB - above copy_file_range threshold
    let content: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let src = setup_source(temp.path(), "large_src.bin", &content);
    let dst = temp.path().join("large_dst.bin");

    let copier = DefaultPlatformCopy::new();
    let result = copier
        .copy_file(&src, &dst, size as u64)
        .expect("copy succeeds");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, content);
    // On macOS with APFS, clonefile copies 0 bytes; otherwise expect full copy
    if result.method != CopyMethod::Clonefile {
        assert_eq!(result.bytes_copied, size as u64);
    }
}

#[test]
fn default_platform_copy_preserves_binary_data() {
    let temp = TempDir::new().expect("create temp dir");
    // Binary content with all byte values
    let content: Vec<u8> = (0..=255).collect();
    let src = setup_source(temp.path(), "binary_src.bin", &content);
    let dst = temp.path().join("binary_dst.bin");

    let copier = DefaultPlatformCopy::new();
    copier
        .copy_file(&src, &dst, content.len() as u64)
        .expect("copy succeeds");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(
        dst_content, content,
        "binary data must be preserved exactly"
    );
}

#[test]
fn default_platform_copy_nonexistent_source() {
    let temp = TempDir::new().expect("create temp dir");
    let src = temp.path().join("nonexistent.txt");
    let dst = temp.path().join("dest.txt");

    let copier = DefaultPlatformCopy::new();
    let result = copier.copy_file(&src, &dst, 0);
    assert!(result.is_err(), "should error on missing source");
}

#[test]
fn default_platform_copy_overwrites_destination() {
    let temp = TempDir::new().expect("create temp dir");
    let src = setup_source(temp.path(), "overwrite_src.txt", b"new content");
    let dst = temp.path().join("overwrite_dst.txt");
    std::fs::write(&dst, b"old content").expect("write old content");

    let copier = DefaultPlatformCopy::new();
    copier.copy_file(&src, &dst, 11).expect("copy succeeds");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, b"new content");
}

#[test]
fn supports_reflink_platform_specific() {
    let copier = DefaultPlatformCopy::new();
    let supports = copier.supports_reflink();

    #[cfg(target_os = "macos")]
    assert!(supports, "macOS should report reflink support");

    #[cfg(target_os = "linux")]
    assert!(supports, "Linux should report reflink support (FICLONE)");

    #[cfg(target_os = "windows")]
    assert!(supports, "Windows should report reflink support (ReFS)");

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    assert!(
        !supports,
        "other platforms should not report reflink support"
    );
}

#[test]
fn preferred_method_small_file() {
    let copier = DefaultPlatformCopy::new();
    let method = copier.preferred_method(100);

    #[cfg(target_os = "macos")]
    assert_eq!(method, CopyMethod::Clonefile);

    #[cfg(target_os = "linux")]
    assert_eq!(method, CopyMethod::Ficlone);

    #[cfg(target_os = "windows")]
    assert_eq!(method, CopyMethod::StandardCopy);
}

#[test]
fn preferred_method_large_file() {
    let copier = DefaultPlatformCopy::new();
    let method = copier.preferred_method(100 * 1024 * 1024); // 100MB

    #[cfg(target_os = "macos")]
    assert_eq!(method, CopyMethod::Clonefile);

    #[cfg(target_os = "linux")]
    assert_eq!(method, CopyMethod::Ficlone);

    #[cfg(target_os = "windows")]
    assert_eq!(method, CopyMethod::CopyFileEx);
}

#[test]
fn trait_object_usage() {
    // Verify PlatformCopy works as a trait object (dyn dispatch)
    let copier: Box<dyn PlatformCopy> = Box::new(DefaultPlatformCopy::new());
    let _supports = copier.supports_reflink();
    let _preferred = copier.preferred_method(1024);
}

#[test]
fn parity_default_vs_std_fs_copy() {
    let temp = TempDir::new().expect("create temp dir");

    let mut content = Vec::new();
    content.extend_from_slice(b"ASCII text\n");
    content.extend_from_slice(&[0x00, 0xFF, 0xAA, 0x55]);
    content.extend_from_slice("Unicode: \u{1F980}\u{1F99E}".as_bytes());

    let src = setup_source(temp.path(), "parity_src.txt", &content);

    // Path 1: PlatformCopy trait
    let dst1 = temp.path().join("parity_dst1.txt");
    let copier = DefaultPlatformCopy::new();
    copier
        .copy_file(&src, &dst1, content.len() as u64)
        .expect("platform copy succeeds");

    // Path 2: std::fs::copy
    let dst2 = temp.path().join("parity_dst2.txt");
    std::fs::copy(&src, &dst2).expect("std::fs::copy succeeds");

    let content1 = std::fs::read(&dst1).expect("read dst1");
    let content2 = std::fs::read(&dst2).expect("read dst2");

    assert_eq!(
        content1, content2,
        "PlatformCopy and std::fs::copy must produce identical output"
    );
    assert_eq!(content1, content, "both must match source");
}

#[cfg(not(target_os = "linux"))]
#[test]
fn ficlone_returns_unsupported_on_non_linux() {
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "ficlone_stub", b"data");

    let err = try_ficlone(&src, &dst).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[cfg(target_os = "linux")]
#[test]
fn ficlone_graceful_fallback_on_tmpfs() {
    // tmpfs does not support reflinks - FICLONE should fail with EOPNOTSUPP.
    // The platform_copy_impl dispatch chain handles this transparently.
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "ficlone_tmpfs", b"test data");

    // Direct FICLONE call - expected to fail on tmpfs/ext4
    let result = try_ficlone(&src, &dst);
    // We don't assert success because CI may run on any filesystem.
    // On Btrfs/XFS: Ok(()), on ext4/tmpfs: Err(EOPNOTSUPP or similar).
    // The key test is that it doesn't panic and returns a clean result.
    match result {
        Ok(()) => {
            // FICLONE succeeded - verify data integrity
            let content = std::fs::read(&dst).expect("read ficlone result");
            assert_eq!(content, b"test data");
        }
        Err(e) => {
            // Expected on non-reflink filesystems
            let raw = e.raw_os_error().unwrap_or(0);
            // EOPNOTSUPP (95), EXDEV (18), EINVAL (22) are all acceptable
            assert!(
                [95, 18, 22].contains(&raw) || e.kind() == io::ErrorKind::Unsupported,
                "unexpected FICLONE error: {e} (raw: {raw})"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn ficlone_fails_on_missing_source() {
    let temp = TempDir::new().expect("create temp dir");
    let src = temp.path().join("nonexistent.txt");
    let dst = temp.path().join("dst.txt");

    let result = try_ficlone(&src, &dst);
    assert!(result.is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn platform_copy_falls_through_ficlone_failure() {
    // Verify the full dispatch chain works: FICLONE fails on tmpfs/ext4,
    // falls through to copy_file_range or std::fs::copy.
    let temp = TempDir::new().expect("create temp dir");
    let content = b"fallback test content";
    let src = setup_source(temp.path(), "ficlone_fallback_src.txt", content);
    let dst = temp.path().join("ficlone_fallback_dst.txt");

    let copier = DefaultPlatformCopy::new();
    let result = copier
        .copy_file(&src, &dst, content.len() as u64)
        .expect("copy should succeed via fallback");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, content);

    // On non-Btrfs, should have fallen back past FICLONE
    // (method will be CopyFileRange or StandardCopy, not Ficlone)
    // On Btrfs, Ficlone is also valid
    assert!(
        matches!(
            result.method,
            CopyMethod::Ficlone | CopyMethod::CopyFileRange | CopyMethod::StandardCopy
        ),
        "unexpected copy method: {:?}",
        result.method
    );
}

#[cfg(not(target_os = "macos"))]
#[test]
fn clonefile_returns_unsupported_on_non_macos() {
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "clone_stub", b"data");

    let err = try_clonefile(&src, &dst).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[cfg(not(target_os = "macos"))]
#[test]
fn fcopyfile_returns_unsupported_on_non_macos() {
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "fcopy_stub", b"data");

    let err = try_fcopyfile(&src, &dst).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[cfg(target_os = "macos")]
#[test]
fn clonefile_copies_data() {
    let temp = TempDir::new().expect("create temp dir");
    let content = b"hello from clonefile";
    let (src, dst) = setup_test_files(temp.path(), "clone_data", content);

    // clonefile requires destination does not exist
    let _ = std::fs::remove_file(&dst);

    match try_clonefile(&src, &dst) {
        Ok(()) => {
            let result = std::fs::read(&dst).expect("read cloned file");
            assert_eq!(result, content);
        }
        Err(e) => {
            // APFS not available (e.g., HFS+ volume) - acceptable in test
            assert_ne!(
                e.kind(),
                io::ErrorKind::Unsupported,
                "macOS should never return Unsupported"
            );
        }
    }
}

#[cfg(target_os = "macos")]
#[test]
fn clonefile_fails_when_dst_exists() {
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "clone_exists", b"data");

    // Create destination so clonefile fails
    std::fs::write(&dst, b"existing").expect("write dst");

    let result = try_clonefile(&src, &dst);
    assert!(result.is_err(), "clonefile should fail when dst exists");
}

#[cfg(target_os = "macos")]
#[test]
fn clonefile_fails_on_missing_source() {
    let temp = TempDir::new().expect("create temp dir");
    let src = temp.path().join("nonexistent.txt");
    let dst = temp.path().join("dst.txt");

    let result = try_clonefile(&src, &dst);
    assert!(result.is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn macos_dispatch_uses_fcopyfile_when_clonefile_fails() {
    // When destination already exists, clonefile will fail. The dispatch
    // chain should then succeed via fcopyfile (reporting CopyMethod::Copyfile).
    let temp = TempDir::new().expect("create temp dir");
    let content = b"dispatch chain test";
    let src = setup_source(temp.path(), "dispatch_src.txt", content);
    let dst = temp.path().join("dispatch_dst.txt");

    // Pre-create destination so clonefile fails (it cannot overwrite)
    std::fs::write(&dst, b"existing").expect("write existing dst");

    let copier = DefaultPlatformCopy::new();
    let result = copier
        .copy_file(&src, &dst, content.len() as u64)
        .expect("dispatch chain should succeed");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, content);

    // Should use fcopyfile (Copyfile) or StandardCopy - not Clonefile
    assert!(
        matches!(
            result.method,
            CopyMethod::Copyfile | CopyMethod::StandardCopy
        ),
        "expected Copyfile or StandardCopy after clonefile failure, got {:?}",
        result.method
    );
}

#[cfg(target_os = "macos")]
#[test]
fn fcopyfile_copies_data() {
    let temp = TempDir::new().expect("create temp dir");
    let content = b"hello from fcopyfile";
    let (src, dst) = setup_test_files(temp.path(), "fcopy_data", content);

    try_fcopyfile(&src, &dst).expect("fcopyfile should succeed");

    let result = std::fs::read(&dst).expect("read copied file");
    assert_eq!(result, content);
}

#[cfg(target_os = "macos")]
#[test]
fn fcopyfile_overwrites_destination() {
    let temp = TempDir::new().expect("create temp dir");
    let content = b"new content";
    let (src, dst) = setup_test_files(temp.path(), "fcopy_overwrite", content);

    // Pre-populate destination
    std::fs::write(&dst, b"old content").expect("write old dst");

    try_fcopyfile(&src, &dst).expect("fcopyfile should succeed");

    let result = std::fs::read(&dst).expect("read copied file");
    assert_eq!(result, content);
}

#[cfg(target_os = "macos")]
#[test]
fn fcopyfile_fails_on_missing_source() {
    let temp = TempDir::new().expect("create temp dir");
    let src = temp.path().join("nonexistent.txt");
    let dst = temp.path().join("dst.txt");

    let result = try_fcopyfile(&src, &dst);
    assert!(result.is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn fcopyfile_copies_empty_file() {
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "fcopy_empty", b"");

    try_fcopyfile(&src, &dst).expect("fcopyfile should succeed for empty file");

    let result = std::fs::read(&dst).expect("read copied file");
    assert!(result.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn fcopyfile_copies_large_file() {
    let temp = TempDir::new().expect("create temp dir");
    let content = vec![0xAB_u8; 1024 * 1024]; // 1MB
    let src = temp.path().join("fcopy_large_src.bin");
    let dst = temp.path().join("fcopy_large_dst.bin");
    std::fs::write(&src, &content).expect("write large source");

    try_fcopyfile(&src, &dst).expect("fcopyfile should succeed for large file");

    let result = std::fs::read(&dst).expect("read large copied file");
    assert_eq!(result.len(), content.len());
    assert_eq!(result, content);
}

#[cfg(target_os = "macos")]
#[test]
fn parity_fcopyfile_vs_std_copy() {
    let temp = TempDir::new().expect("create temp dir");

    let mut content = Vec::new();
    content.extend_from_slice(b"ASCII text\n");
    content.extend_from_slice(&[0x00, 0xFF, 0xAA, 0x55]);
    content.extend_from_slice("Unicode: \u{1F980}\u{1F99E}".as_bytes());

    let src = temp.path().join("parity_src.bin");
    std::fs::write(&src, &content).expect("write source");

    let dst_fcopy = temp.path().join("parity_fcopy.bin");
    try_fcopyfile(&src, &dst_fcopy).expect("fcopyfile should succeed");

    let dst_std = temp.path().join("parity_std.bin");
    std::fs::copy(&src, &dst_std).expect("std::fs::copy should succeed");

    let result_fcopy = std::fs::read(&dst_fcopy).expect("read fcopyfile result");
    let result_std = std::fs::read(&dst_std).expect("read std::fs::copy result");

    assert_eq!(
        result_fcopy, result_std,
        "fcopyfile and std::fs::copy must produce identical output"
    );
    assert_eq!(result_fcopy, content);
}

#[cfg(not(target_os = "windows"))]
#[test]
fn refs_reflink_returns_unsupported_on_non_windows() {
    let temp = TempDir::new().expect("create temp dir");
    let (src, dst) = setup_test_files(temp.path(), "refs_stub", b"data");

    let err = try_refs_reflink(&src, &dst).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
}

#[cfg(target_os = "windows")]
#[test]
fn refs_reflink_fails_gracefully_on_ntfs() {
    // Standard Windows CI runners use NTFS, not ReFS.
    // The reflink attempt should fail with a clean error, not panic.
    let temp = TempDir::new().expect("create temp dir");
    let content = b"reflink test data on NTFS";
    let (src, dst) = setup_test_files(temp.path(), "refs_ntfs", content);

    let result = try_refs_reflink(&src, &dst);
    // NTFS does not support FSCTL_DUPLICATE_EXTENTS - expect an error
    assert!(
        result.is_err(),
        "reflink should fail on NTFS (CI uses NTFS)"
    );
}

#[cfg(target_os = "windows")]
#[test]
fn refs_reflink_fails_on_missing_source() {
    let temp = TempDir::new().expect("create temp dir");
    let src = temp.path().join("nonexistent.txt");
    let dst = temp.path().join("dst.txt");

    let result = try_refs_reflink(&src, &dst);
    assert!(result.is_err());
}

#[cfg(target_os = "windows")]
#[test]
fn dispatch_falls_back_from_reflink_on_ntfs() {
    // When is_refs returns false (NTFS), the dispatch chain should skip
    // reflink and proceed to CopyFileExW or standard copy.
    let temp = TempDir::new().expect("create temp dir");
    let content = b"fallback test content";
    let src = setup_source(temp.path(), "refs_fallback_src.txt", content);
    let dst = temp.path().join("refs_fallback_dst.txt");

    let copier = DefaultPlatformCopy::new();
    let result = copier
        .copy_file(&src, &dst, content.len() as u64)
        .expect("copy should succeed via CopyFileExW fallback");

    let dst_content = std::fs::read(&dst).expect("read destination");
    assert_eq!(dst_content, content);

    // On NTFS, should use CopyFileEx or StandardCopy (not ReFsReflink)
    assert!(
        matches!(
            result.method,
            CopyMethod::CopyFileEx | CopyMethod::StandardCopy
        ),
        "expected CopyFileEx or StandardCopy on NTFS, got {:?}",
        result.method
    );
}
