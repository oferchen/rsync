use super::*;
use crate::signature::SignatureAlgorithm;
use ::metadata::ChmodModifiers;
use bandwidth::BandwidthLimiter;
use compress::algorithm::CompressionAlgorithm;
use compress::zlib::CompressionLevel;
use filetime::{FileTime, set_file_mtime, set_file_times};
use filters::{FilterRule, FilterSet};
use std::ffi::{OsStr, OsString};
use std::fs;
#[cfg(unix)]
use std::io::Read;
use std::io::{self, Write};
#[cfg(unix)]
use std::io::{Seek, SeekFrom};
use std::num::{NonZeroU8, NonZeroU32, NonZeroU64};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;
use tempfile::tempdir;

mod deletion_strategies;

#[cfg(all(unix, feature = "xattr"))]
use xattr;

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    use std::convert::TryInto;

    let bits: u16 = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    let mode = Mode::from_bits_truncate(bits.into());
    mknodat(CWD, path, FileType::Fifo, mode, makedev(0, 0)).map_err(io::Error::from)
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn mkfifo_for_tests(path: &Path, mode: u32) -> io::Result<()> {
    use std::convert::TryInto;

    let bits: libc::mode_t = (mode & 0o177_777)
        .try_into()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "mode out of range"))?;
    apple_fs::mkfifo(path, bits)
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
use std::os::unix::ffi::OsStrExt;

#[cfg(all(unix, feature = "acl"))]
mod acl_sys {
    #![allow(unsafe_code, dead_code)]

    use libc::{c_char, c_int, c_void, ssize_t};

    pub type AclHandle = *mut c_void;
    pub type AclType = c_int;
    pub const ACL_TYPE_ACCESS: AclType = 0x8000;

    #[cfg_attr(not(target_vendor = "apple"), link(name = "acl"))]
    unsafe extern "C" {
        pub fn acl_get_file(path_p: *const c_char, ty: AclType) -> AclHandle;
        pub fn acl_set_file(path_p: *const c_char, ty: AclType, acl: AclHandle) -> c_int;
        pub fn acl_to_text(acl: AclHandle, len_p: *mut ssize_t) -> *mut c_char;
        pub fn acl_from_text(buf_p: *const c_char) -> AclHandle;
        pub fn acl_free(obj_p: *mut c_void) -> c_int;
    }

    pub unsafe fn free(handle: AclHandle) {
        // Safety: callers ensure the pointer originates from libacl.
        let _ = unsafe { acl_free(handle) };
    }

    pub unsafe fn free_text(handle: *mut c_char) {
        // Safety: callers ensure the pointer originates from libacl.
        let _ = unsafe { acl_free(handle.cast()) };
    }
}

#[cfg(all(unix, feature = "acl"))]
trait AclStrategy {
    /// Return a textual representation of the ACL for `path`, or `None` if
    /// no ACL is present or ACLs are effectively unsupported.
    fn get_text(&self, path: &Path, ty: acl_sys::AclType) -> Option<String>;

    /// Apply an ACL described by `text` to `path`. Implementations may be
    /// no-ops on platforms where ACLs are stubbed.
    fn set_from_text(&self, path: &Path, text: &str, ty: acl_sys::AclType);
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
struct LibAclStrategy;

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
impl AclStrategy for LibAclStrategy {
    fn get_text(&self, path: &Path, ty: acl_sys::AclType) -> Option<String> {
        use std::ffi::CString;
        use std::slice;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring path");
        // Safety: `c_path` is a valid, NUL-terminated C string.
        let acl = unsafe { acl_sys::acl_get_file(c_path.as_ptr(), ty) };
        if acl.is_null() {
            return None;
        }

        let mut len = 0;
        // Safety: `acl` comes from `acl_get_file` and remains valid.
        let text_ptr = unsafe { acl_sys::acl_to_text(acl, &mut len) };
        if text_ptr.is_null() {
            unsafe { acl_sys::free(acl) };
            return None;
        }

        // Safety: `text_ptr` and `len` are provided by libacl.
        let slice = unsafe { slice::from_raw_parts(text_ptr.cast::<u8>(), len as usize) };
        let text = String::from_utf8_lossy(slice).trim().to_owned();

        unsafe {
            acl_sys::free_text(text_ptr);
            acl_sys::free(acl);
        }

        if text.is_empty() { None } else { Some(text) }
    }

    fn set_from_text(&self, path: &Path, text: &str, ty: acl_sys::AclType) {
        use std::ffi::CString;

        let c_path = CString::new(path.as_os_str().as_bytes()).expect("cstring path");
        let c_text = CString::new(text).expect("cstring text");

        // Safety: both CStrings are valid, NUL-terminated byte strings.
        let acl = unsafe { acl_sys::acl_from_text(c_text.as_ptr()) };
        // On non-Apple Unix we require the text representation to be valid.
        assert!(!acl.is_null(), "acl_from_text");

        // Safety: `acl` comes from libacl and remains valid during the call.
        let result = unsafe { acl_sys::acl_set_file(c_path.as_ptr(), ty, acl) };
        unsafe {
            acl_sys::free(acl);
        }
        assert_eq!(result, 0, "acl_set_file");
    }
}

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
struct NoOpAclStrategy;

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
impl AclStrategy for NoOpAclStrategy {
    fn get_text(&self, _path: &Path, _ty: acl_sys::AclType) -> Option<String> {
        // Apple platforms follow the metadata crate's ACL stub: ACL
        // support is effectively unavailable, but the pipeline must not
        // crash or fail when ACLs are requested.
        None
    }

    fn set_from_text(&self, _path: &Path, _text: &str, _ty: acl_sys::AclType) {
        // No-op stub: behave as if ACLs are simply not preserved.
    }
}

#[cfg(all(unix, feature = "acl", not(target_vendor = "apple")))]
static ACTIVE_ACL_STRATEGY: LibAclStrategy = LibAclStrategy;

#[cfg(all(unix, feature = "acl", target_vendor = "apple"))]
static ACTIVE_ACL_STRATEGY: NoOpAclStrategy = NoOpAclStrategy;

#[cfg(all(unix, feature = "acl"))]
fn active_acl_strategy() -> &'static dyn AclStrategy {
    &ACTIVE_ACL_STRATEGY
}

#[cfg(all(unix, feature = "acl"))]
fn acl_to_text(path: &Path, ty: acl_sys::AclType) -> Option<String> {
    active_acl_strategy().get_text(path, ty)
}

#[cfg(all(unix, feature = "acl"))]
fn set_acl_from_text(path: &Path, text: &str, ty: acl_sys::AclType) {
    active_acl_strategy().set_from_text(path, text, ty)
}

mod test_helpers {
    use filetime::FileTime;
    use std::path::{Path, PathBuf};
    use tempfile::TempDir;

    // ==================== Test Context ====================

    /// Test context for copy operations.
    ///
    /// This structure provides a self-contained test environment with:
    /// - A temporary directory that is automatically cleaned up
    /// - Pre-configured source and destination paths
    /// - Utility methods for common operations
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = test_helpers::setup_copy_test();
    /// // ctx.source is already created
    /// // ctx.dest is NOT created (allows testing directory creation)
    /// std::fs::write(ctx.source.join("file.txt"), b"content").unwrap();
    /// ```
    pub struct CopyTestContext {
        /// Temporary directory that must be kept alive for paths to remain valid.
        #[allow(dead_code)]
        pub temp_dir: TempDir,
        /// Source directory path (already created).
        pub source: PathBuf,
        /// Destination directory path (NOT created by default).
        pub dest: PathBuf,
    }

    impl CopyTestContext {
        /// Returns an additional path within the temp directory.
        ///
        /// Useful for creating reference directories, backup directories, etc.
        #[allow(dead_code)]
        pub fn additional_path(&self, name: &str) -> PathBuf {
            self.temp_dir.path().join(name)
        }

        /// Creates a file in the source directory with the given content.
        #[allow(dead_code)]
        pub fn write_source(&self, relative_path: &str, content: &[u8]) {
            let path = self.source.join(relative_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent directories");
            }
            std::fs::write(&path, content).expect("write source file");
        }

        /// Creates a file in the destination directory with the given content.
        ///
        /// Also creates the destination directory if it doesn't exist.
        #[allow(dead_code)]
        pub fn write_dest(&self, relative_path: &str, content: &[u8]) {
            let path = self.dest.join(relative_path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("create parent directories");
            }
            std::fs::write(&path, content).expect("write dest file");
        }

        /// Reads content from a file in the destination directory.
        #[allow(dead_code)]
        pub fn read_dest(&self, relative_path: &str) -> Vec<u8> {
            std::fs::read(self.dest.join(relative_path)).expect("read dest file")
        }

        /// Checks if a file exists in the destination directory.
        #[allow(dead_code)]
        pub fn dest_exists(&self, relative_path: &str) -> bool {
            self.dest.join(relative_path).exists()
        }

        /// Checks if a path is a directory in the destination.
        #[allow(dead_code)]
        pub fn dest_is_dir(&self, relative_path: &str) -> bool {
            self.dest.join(relative_path).is_dir()
        }

        /// Checks if a path is a file in the destination.
        #[allow(dead_code)]
        pub fn dest_is_file(&self, relative_path: &str) -> bool {
            self.dest.join(relative_path).is_file()
        }

        /// Gets operands for LocalCopyPlan::from_operands.
        ///
        /// Returns `[source, dest]` as OsStrings.
        #[allow(dead_code)]
        pub fn operands(&self) -> Vec<std::ffi::OsString> {
            vec![
                self.source.clone().into_os_string(),
                self.dest.clone().into_os_string(),
            ]
        }

        /// Gets operands with trailing separator on source (copies contents).
        ///
        /// This is equivalent to `rsync source/ dest` which copies the
        /// contents of source into dest rather than source itself.
        #[allow(dead_code)]
        pub fn operands_with_trailing_separator(&self) -> Vec<std::ffi::OsString> {
            let mut source_operand = self.source.clone().into_os_string();
            source_operand.push(std::path::MAIN_SEPARATOR.to_string());
            vec![source_operand, self.dest.clone().into_os_string()]
        }
    }

    // ==================== Setup Functions ====================

    /// Creates a test context with temporary directory and source/dest paths.
    ///
    /// The source directory is created automatically. The dest directory is not
    /// created to allow tests to control whether it exists.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let ctx = test_helpers::setup_copy_test();
    /// std::fs::write(ctx.source.join("file.txt"), b"hello").unwrap();
    /// // ... run copy operation ...
    /// assert!(ctx.dest.join("source/file.txt").exists());
    /// ```
    pub fn setup_copy_test() -> CopyTestContext {
        let temp_dir = tempfile::tempdir().expect("tempdir");
        let source = temp_dir.path().join("source");
        let dest = temp_dir.path().join("dest");
        std::fs::create_dir_all(&source).expect("create source");
        CopyTestContext {
            temp_dir,
            source,
            dest,
        }
    }

    /// Creates a test context with both source and destination pre-created.
    ///
    /// Use this when testing scenarios that require an existing destination.
    #[allow(dead_code)]
    pub fn setup_copy_test_with_dest() -> CopyTestContext {
        let ctx = setup_copy_test();
        std::fs::create_dir_all(&ctx.dest).expect("create dest");
        ctx
    }

    /// Creates a test context with a reference directory for --link-dest tests.
    ///
    /// Returns (context, reference_path) where reference_path is an additional
    /// directory that can be used as a link-dest reference.
    #[allow(dead_code)]
    pub fn setup_link_dest_test() -> (CopyTestContext, PathBuf) {
        let ctx = setup_copy_test();
        let reference = ctx.additional_path("reference");
        std::fs::create_dir_all(&reference).expect("create reference");
        (ctx, reference)
    }

    // ==================== Tree Creation ====================

    /// Creates a directory tree for testing.
    ///
    /// Takes a base path and a specification of paths to create. Each entry is a
    /// tuple of (path, optional_content):
    /// - If `content` is `Some(data)`, a file is created with that content
    /// - If `content` is `None`, an empty directory is created
    ///
    /// Parent directories are created automatically as needed.
    ///
    /// # Examples
    ///
    /// ```ignore
    /// let temp = tempdir().expect("tempdir");
    /// create_test_tree(temp.path(), &[
    ///     ("dir1/file1.txt", Some(b"content1")),
    ///     ("dir1/file2.txt", Some(b"content2")),
    ///     ("dir2/subdir", None),  // empty directory
    ///     ("dir3/nested/file.txt", Some(b"nested")),
    /// ]);
    /// ```
    #[allow(dead_code)]
    pub fn create_test_tree(base: &Path, spec: &[(&str, Option<&[u8]>)]) {
        for (path, content) in spec {
            let full_path = base.join(path);
            if let Some(data) = content {
                if let Some(parent) = full_path.parent() {
                    std::fs::create_dir_all(parent).expect("create parent directories");
                }
                std::fs::write(&full_path, data).expect("write file");
            } else {
                std::fs::create_dir_all(&full_path).expect("create directory");
            }
        }
    }

    /// Creates a file with specific size (filled with a repeating byte).
    ///
    /// Useful for testing min-size/max-size filters.
    #[allow(dead_code)]
    pub fn create_file_with_size(path: &Path, size: usize, fill_byte: u8) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories");
        }
        std::fs::write(path, vec![fill_byte; size]).expect("write file");
    }

    /// Creates a file with a specific modification time.
    #[allow(dead_code)]
    pub fn create_file_with_mtime(path: &Path, content: &[u8], unix_timestamp: i64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories");
        }
        std::fs::write(path, content).expect("write file");
        let mtime = FileTime::from_unix_time(unix_timestamp, 0);
        filetime::set_file_mtime(path, mtime).expect("set mtime");
    }

    /// Creates a file with both access and modification times set.
    #[allow(dead_code)]
    pub fn create_file_with_times(path: &Path, content: &[u8], atime_unix: i64, mtime_unix: i64) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories");
        }
        std::fs::write(path, content).expect("write file");
        let atime = FileTime::from_unix_time(atime_unix, 0);
        let mtime = FileTime::from_unix_time(mtime_unix, 0);
        filetime::set_file_times(path, atime, mtime).expect("set times");
    }

    // ==================== Unix-specific Helpers ====================

    /// Creates a symbolic link (Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn create_symlink(target: &Path, link_path: &Path) {
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories");
        }
        std::os::unix::fs::symlink(target, link_path).expect("create symlink");
    }

    /// Creates a symbolic link with a relative target (Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn create_relative_symlink(relative_target: &str, link_path: &Path) {
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories");
        }
        std::os::unix::fs::symlink(Path::new(relative_target), link_path).expect("create symlink");
    }

    /// Creates a hard link (Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn create_hard_link(original: &Path, link_path: &Path) {
        if let Some(parent) = link_path.parent() {
            std::fs::create_dir_all(parent).expect("create parent directories");
        }
        std::fs::hard_link(original, link_path).expect("create hard link");
    }

    /// Sets Unix file permissions.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn set_permissions(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(mode);
        std::fs::set_permissions(path, perms).expect("set permissions");
    }

    /// Gets Unix file permissions mode.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn get_permissions(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .expect("get metadata")
            .permissions()
            .mode()
            & 0o7777
    }

    /// Checks if two files share the same inode (are hard linked).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn same_inode(path1: &Path, path2: &Path) -> bool {
        use std::os::unix::fs::MetadataExt;
        let meta1 = std::fs::metadata(path1).expect("metadata 1");
        let meta2 = std::fs::metadata(path2).expect("metadata 2");
        meta1.ino() == meta2.ino() && meta1.dev() == meta2.dev()
    }

    /// Gets the inode number of a file.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn get_inode(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).expect("metadata").ino()
    }

    /// Gets the link count (nlink) of a file.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn get_nlink(path: &Path) -> u64 {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).expect("metadata").nlink()
    }

    // ==================== Timestamp Helpers ====================

    /// A commonly used timestamp for tests (roughly 2023-11-14).
    pub const TEST_TIMESTAMP: i64 = 1_700_000_000;

    /// Returns a FileTime at a fixed test timestamp.
    #[allow(dead_code)]
    pub fn test_time() -> FileTime {
        FileTime::from_unix_time(TEST_TIMESTAMP, 0)
    }

    /// Returns a FileTime offset from the test timestamp by the given seconds.
    #[allow(dead_code)]
    pub fn test_time_offset(seconds: i64) -> FileTime {
        FileTime::from_unix_time(TEST_TIMESTAMP + seconds, 0)
    }

    /// Sets the modification time of a file to the test timestamp.
    #[allow(dead_code)]
    pub fn set_test_mtime(path: &Path) {
        filetime::set_file_mtime(path, test_time()).expect("set mtime");
    }

    /// Synchronizes the modification time of two files.
    ///
    /// Makes `dest` have the same mtime as `source`.
    #[allow(dead_code)]
    pub fn sync_mtime(source: &Path, dest: &Path) {
        let mtime = FileTime::from_last_modification_time(
            &std::fs::metadata(source).expect("source metadata"),
        );
        filetime::set_file_mtime(dest, mtime).expect("set dest mtime");
    }

    /// Gets the modification time of a file as a FileTime.
    #[allow(dead_code)]
    pub fn get_mtime(path: &Path) -> FileTime {
        FileTime::from_last_modification_time(&std::fs::metadata(path).expect("metadata"))
    }

    // ==================== Assertion Helpers ====================

    /// Asserts that a file exists at the given path.
    #[allow(dead_code)]
    pub fn assert_file_exists(path: &Path) {
        assert!(path.exists(), "expected file to exist: {}", path.display());
        assert!(
            path.is_file(),
            "expected path to be a file: {}",
            path.display()
        );
    }

    /// Asserts that a file does not exist at the given path.
    #[allow(dead_code)]
    pub fn assert_file_not_exists(path: &Path) {
        assert!(
            !path.exists(),
            "expected file to not exist: {}",
            path.display()
        );
    }

    /// Asserts that a directory exists at the given path.
    #[allow(dead_code)]
    pub fn assert_dir_exists(path: &Path) {
        assert!(
            path.exists(),
            "expected directory to exist: {}",
            path.display()
        );
        assert!(
            path.is_dir(),
            "expected path to be a directory: {}",
            path.display()
        );
    }

    /// Asserts that a directory does not exist at the given path.
    #[allow(dead_code)]
    pub fn assert_dir_not_exists(path: &Path) {
        assert!(
            !path.exists(),
            "expected directory to not exist: {}",
            path.display()
        );
    }

    /// Asserts that a file has the expected content.
    #[allow(dead_code)]
    pub fn assert_file_content(path: &Path, expected: &[u8]) {
        let actual =
            std::fs::read(path).unwrap_or_else(|_| panic!("read file: {}", path.display()));
        assert_eq!(
            actual,
            expected,
            "file content mismatch for {}",
            path.display()
        );
    }

    /// Asserts that a file has the expected size.
    #[allow(dead_code)]
    pub fn assert_file_size(path: &Path, expected_size: u64) {
        let actual = std::fs::metadata(path)
            .unwrap_or_else(|_| panic!("metadata: {}", path.display()))
            .len();
        assert_eq!(
            actual,
            expected_size,
            "file size mismatch for {}: expected {}, got {}",
            path.display(),
            expected_size,
            actual
        );
    }

    /// Asserts that two files have identical content.
    #[allow(dead_code)]
    pub fn assert_files_equal(path1: &Path, path2: &Path) {
        let content1 = std::fs::read(path1).unwrap_or_else(|_| panic!("read: {}", path1.display()));
        let content2 = std::fs::read(path2).unwrap_or_else(|_| panic!("read: {}", path2.display()));
        assert_eq!(
            content1,
            content2,
            "file content mismatch between {} and {}",
            path1.display(),
            path2.display()
        );
    }

    /// Asserts that a path is a symlink (Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn assert_is_symlink(path: &Path) {
        let meta = std::fs::symlink_metadata(path)
            .unwrap_or_else(|_| panic!("symlink_metadata: {}", path.display()));
        assert!(
            meta.file_type().is_symlink(),
            "expected {} to be a symlink",
            path.display()
        );
    }

    /// Asserts that a symlink points to the expected target (Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn assert_symlink_target(link_path: &Path, expected_target: &Path) {
        let actual_target = std::fs::read_link(link_path)
            .unwrap_or_else(|_| panic!("read_link: {}", link_path.display()));
        assert_eq!(
            actual_target,
            expected_target,
            "symlink target mismatch for {}",
            link_path.display()
        );
    }

    /// Asserts that two files are hard linked (same inode, Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn assert_hard_linked(path1: &Path, path2: &Path) {
        assert!(
            same_inode(path1, path2),
            "expected {} and {} to be hard linked (same inode)",
            path1.display(),
            path2.display()
        );
    }

    /// Asserts that two files are NOT hard linked (different inodes, Unix only).
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn assert_not_hard_linked(path1: &Path, path2: &Path) {
        assert!(
            !same_inode(path1, path2),
            "expected {} and {} to NOT be hard linked",
            path1.display(),
            path2.display()
        );
    }

    /// Asserts that a file has the expected Unix permissions mode.
    #[cfg(unix)]
    #[allow(dead_code)]
    pub fn assert_permissions(path: &Path, expected_mode: u32) {
        let actual = get_permissions(path);
        assert_eq!(
            actual & 0o7777,
            expected_mode & 0o7777,
            "permissions mismatch for {}: expected {:o}, got {:o}",
            path.display(),
            expected_mode,
            actual
        );
    }

    /// Asserts that a file has the expected modification time.
    #[allow(dead_code)]
    pub fn assert_mtime(path: &Path, expected: FileTime) {
        let actual = get_mtime(path);
        assert_eq!(
            actual,
            expected,
            "mtime mismatch for {}: expected {:?}, got {:?}",
            path.display(),
            expected,
            actual
        );
    }

    // ==================== Summary Assertion Helpers ====================

    /// Builder for asserting copy summary statistics.
    ///
    /// # Example
    ///
    /// ```ignore
    /// test_helpers::SummaryAssertions::new(&summary)
    ///     .files_copied(2)
    ///     .files_matched(1)
    ///     .items_deleted(0)
    ///     .assert();
    /// ```
    #[allow(dead_code)]
    pub struct SummaryAssertions<'a> {
        summary: &'a super::LocalCopySummary,
        expected_files_copied: Option<u64>,
        expected_files_matched: Option<u64>,
        expected_bytes_copied: Option<u64>,
        expected_items_deleted: Option<u64>,
        expected_directories_created: Option<u64>,
        expected_symlinks_copied: Option<u64>,
        expected_hard_links_created: Option<u64>,
    }

    #[allow(dead_code)]
    impl<'a> SummaryAssertions<'a> {
        pub fn new(summary: &'a super::LocalCopySummary) -> Self {
            Self {
                summary,
                expected_files_copied: None,
                expected_files_matched: None,
                expected_bytes_copied: None,
                expected_items_deleted: None,
                expected_directories_created: None,
                expected_symlinks_copied: None,
                expected_hard_links_created: None,
            }
        }

        pub fn files_copied(mut self, count: u64) -> Self {
            self.expected_files_copied = Some(count);
            self
        }

        pub fn files_matched(mut self, count: u64) -> Self {
            self.expected_files_matched = Some(count);
            self
        }

        pub fn bytes_copied(mut self, count: u64) -> Self {
            self.expected_bytes_copied = Some(count);
            self
        }

        pub fn items_deleted(mut self, count: u64) -> Self {
            self.expected_items_deleted = Some(count);
            self
        }

        pub fn directories_created(mut self, count: u64) -> Self {
            self.expected_directories_created = Some(count);
            self
        }

        pub fn directories_created_at_least(mut self, min_count: u64) -> Self {
            // Special handling: we store the minimum and check >= in assert
            self.expected_directories_created = Some(min_count | (1 << 63));
            self
        }

        pub fn symlinks_copied(mut self, count: u64) -> Self {
            self.expected_symlinks_copied = Some(count);
            self
        }

        pub fn hard_links_created(mut self, count: u64) -> Self {
            self.expected_hard_links_created = Some(count);
            self
        }

        pub fn hard_links_created_at_least(mut self, min_count: u64) -> Self {
            self.expected_hard_links_created = Some(min_count | (1 << 63));
            self
        }

        pub fn assert(self) {
            if let Some(expected) = self.expected_files_copied {
                assert_eq!(
                    self.summary.files_copied(),
                    expected,
                    "files_copied mismatch"
                );
            }
            if let Some(expected) = self.expected_files_matched {
                assert_eq!(
                    self.summary.regular_files_matched(),
                    expected,
                    "regular_files_matched mismatch"
                );
            }
            if let Some(expected) = self.expected_bytes_copied {
                assert_eq!(
                    self.summary.bytes_copied(),
                    expected,
                    "bytes_copied mismatch"
                );
            }
            if let Some(expected) = self.expected_items_deleted {
                assert_eq!(
                    self.summary.items_deleted(),
                    expected,
                    "items_deleted mismatch"
                );
            }
            if let Some(expected) = self.expected_directories_created {
                if expected & (1 << 63) != 0 {
                    let min = expected & !(1 << 63);
                    assert!(
                        self.summary.directories_created() >= min,
                        "directories_created should be at least {}, got {}",
                        min,
                        self.summary.directories_created()
                    );
                } else {
                    assert_eq!(
                        self.summary.directories_created(),
                        expected,
                        "directories_created mismatch"
                    );
                }
            }
            if let Some(expected) = self.expected_symlinks_copied {
                assert_eq!(
                    self.summary.symlinks_copied(),
                    expected,
                    "symlinks_copied mismatch"
                );
            }
            if let Some(expected) = self.expected_hard_links_created {
                if expected & (1 << 63) != 0 {
                    let min = expected & !(1 << 63);
                    assert!(
                        self.summary.hard_links_created() >= min,
                        "hard_links_created should be at least {}, got {}",
                        min,
                        self.summary.hard_links_created()
                    );
                } else {
                    assert_eq!(
                        self.summary.hard_links_created(),
                        expected,
                        "hard_links_created mismatch"
                    );
                }
            }
        }
    }

    // ==================== Options Builder Helpers ====================

    /// Common option presets for tests.
    pub mod presets {
        use super::super::{FilterSet, LocalCopyOptions};
        use filters::FilterRule;

        /// Options for archive-like copy (-a equivalent).
        ///
        /// Enables: recursive, links, permissions, times, group, owner, devices, specials
        #[allow(dead_code)]
        pub fn archive_options() -> LocalCopyOptions {
            LocalCopyOptions::default()
                .recursive(true)
                .links(true)
                .permissions(true)
                .times(true)
                .group(true)
                .owner(true)
                .devices(true)
                .specials(true)
        }

        /// Options for basic copy with times preservation.
        #[allow(dead_code)]
        pub fn basic_with_times() -> LocalCopyOptions {
            LocalCopyOptions::default().times(true)
        }

        /// Options for copy with checksum comparison.
        #[allow(dead_code)]
        pub fn with_checksum() -> LocalCopyOptions {
            LocalCopyOptions::default().checksum(true)
        }

        /// Options for incremental backup (delete + backup).
        #[allow(dead_code)]
        pub fn incremental_backup() -> LocalCopyOptions {
            LocalCopyOptions::default().delete(true).backup(true)
        }

        /// Options for mirror copy (delete + times + permissions).
        #[allow(dead_code)]
        pub fn mirror() -> LocalCopyOptions {
            LocalCopyOptions::default()
                .delete(true)
                .times(true)
                .permissions(true)
        }

        /// Options excluding common temporary files.
        #[allow(dead_code)]
        pub fn exclude_temp_files() -> LocalCopyOptions {
            let filters = FilterSet::from_rules([
                FilterRule::exclude("*.tmp"),
                FilterRule::exclude("*.bak"),
                FilterRule::exclude("*~"),
            ])
            .expect("compile temp file filters");
            LocalCopyOptions::default().filters(Some(filters))
        }

        /// Options for hard link preservation.
        #[allow(dead_code)]
        pub fn preserve_hard_links() -> LocalCopyOptions {
            LocalCopyOptions::default().hard_links(true)
        }

        /// Options for symbolic link preservation.
        #[allow(dead_code)]
        pub fn preserve_symlinks() -> LocalCopyOptions {
            LocalCopyOptions::default().links(true)
        }

        /// Options for safe symbolic link handling.
        #[allow(dead_code)]
        pub fn safe_symlinks() -> LocalCopyOptions {
            LocalCopyOptions::default().links(true).safe_links(true)
        }
    }
}

#[cfg(unix)]
mod unix_ids {
    #![allow(unsafe_code)]

    pub(super) fn uid(raw: u32) -> rustix::fs::Uid {
        // Safety: constructing `Uid` from a raw value is how rustix exposes platform IDs.
        unsafe { rustix::fs::Uid::from_raw(raw) }
    }

    pub(super) fn gid(raw: u32) -> rustix::fs::Gid {
        // Safety: constructing `Gid` from a raw value is how rustix exposes platform IDs.
        unsafe { rustix::fs::Gid::from_raw(raw) }
    }
}

include!("options.rs");
include!("filters.rs");
include!("relative.rs");
include!("plan.rs");
include!("execute_append.rs");
include!("execute_basic.rs");
include!("execute_skip.rs");
include!("execute_ignore_existing.rs");
include!("execute_existing.rs");
include!("execute_delta.rs");
include!("execute_whole_file.rs");
include!("execute_symlinks.rs");
include!("execute_symlink_edge_cases.rs");
include!("execute_copy_links.rs");
include!("execute_dirlinks.rs");
include!("execute_metadata.rs");
include!("execute_executability.rs");
include!("execute_special.rs");
include!("execute_specials.rs");
include!("execute_directories.rs");
include!("execute_prune_empty_dirs.rs");
include!("execute_hardlinks.rs");
include!("execute_link_dest.rs");
include!("execute_copy_dest.rs");
include!("execute_sparse.rs");
include!("execute_min_size.rs");
include!("max_size_filter.rs");
include!("bandwidth.rs");
include!("filters_runtime.rs");
include!("delete.rs");
include!("backups.rs");
include!("delete_protect.rs");
include!("dest_guard.rs");
include!("executor_file_comparison.rs");
include!("executor_file_operations.rs");
include!("execute_modify_window.rs");
include!("execute_one_file_system.rs");
include!("execute_omit_dir_times.rs");

#[cfg(unix)]
include!("execute_fsync.rs");
#[cfg(unix)]
include!("execute_numeric_ids.rs");

// Additional test modules
include!("delete_delay.rs");
#[cfg(unix)]
include!("execute_chown.rs");
include!("execute_compare_dest.rs");
include!("execute_delay_updates.rs");
include!("execute_ignore_times.rs");
include!("execute_inplace.rs");
include!("execute_no_implied_dirs.rs");
include!("execute_permissions.rs");
include!("execute_remove_source.rs");
include!("execute_temp_dir.rs");
include!("execute_update.rs");
#[cfg(unix)]
include!("execute_usermap_groupmap.rs");
include!("execute_zero_length.rs");
include!("execute_max_delete.rs");
include!("itemize_changes.rs");
include!("list_only.rs");
include!("partial_dir.rs");
include!("partial_transfers.rs");
include!("checksum_algorithm_behavior.rs");
include!("execute_timestamp_preservation.rs");
include!("disk_full_errors.rs");
#[cfg(unix)]
include!("permission_errors.rs");
include!("concurrent_modification.rs");
#[cfg(unix)]
include!("execute_ownership_preservation.rs");
include!("execute_xattrs.rs");
#[cfg(unix)]
include!("execute_special_characters.rs");
include!("execute_force.rs");
include!("execute_long_paths.rs");
include!("execute_block_size.rs");
include!("execute_super.rs");
include!("execute_archive.rs");
include!("execute_ignore_errors.rs");
include!("execute_checksum_seed.rs");
include!("timeout_handling.rs");
include!("execute_contimeout.rs");
include!("execute_log_file.rs");
include!("execute_filter_program.rs");
include!("execute_skip_compress.rs");
include!("execute_dry_run.rs");
