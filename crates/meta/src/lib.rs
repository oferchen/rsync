#![deny(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]

//! # Overview
//!
//! `rsync_meta` centralises metadata preservation helpers used by the Rust
//! rsync workspace. The crate focuses on reproducing upstream `rsync`
//! semantics for permission bits and timestamp propagation when copying files,
//! directories, symbolic links, device nodes, and FIFOs on local filesystems.
//! Higher layers wire the helpers into transfer pipelines so metadata handling
//! remains consistent across client and daemon roles.
//!
//! # Design
//!
//! The crate exposes three primary entry points:
//! - [`apply_file_metadata`] sets permissions and timestamps on regular files.
//! - [`apply_directory_metadata`] mirrors metadata for directories.
//! - [`apply_symlink_metadata`] applies timestamp changes to symbolic links
//!   without following the link target.
//! - [`create_fifo`] materialises FIFOs before metadata is applied, allowing
//!   higher layers to reproduce upstream handling of named pipes.
//! - [`create_device_node`] builds character and block device nodes from the
//!   metadata observed on the source filesystem so downstream code can
//!   faithfully mirror special files during local copies.
//!
//! Errors are reported via [`MetadataError`], which stores the failing path and
//! operation context. Callers can integrate the error into user-facing
//! diagnostics while retaining the underlying [`io::Error`].
//!
//! # Invariants
//!
//! - All helpers avoid following symbolic links unless explicitly requested.
//! - Permission preservation is best-effort on non-Unix platforms where only
//!   the read-only flag may be applied.
//! - Timestamp propagation always uses nanosecond precision via the
//!   [`filetime`] crate.
//!
//! # Errors
//!
//! Operations surface [`MetadataError`] when the underlying filesystem call
//! fails. The error exposes the context string, path, and original [`io::Error`]
//! so higher layers can render diagnostics consistent with upstream `rsync`.
//!
//! # Examples
//!
//! ```
//! use rsync_meta::{apply_file_metadata, MetadataError};
//! use std::fs;
//! use std::path::Path;
//!
//! # fn demo() -> Result<(), MetadataError> {
//! let source = Path::new("source.txt");
//! let dest = Path::new("dest.txt");
//! fs::write(source, b"data").expect("write source");
//! fs::write(dest, b"data").expect("write dest");
//! let metadata = fs::metadata(source).expect("source metadata");
//! apply_file_metadata(dest, &metadata)?;
//! # fs::remove_file(source).expect("remove source");
//! # fs::remove_file(dest).expect("remove dest");
//! Ok(())
//! # }
//! # demo().unwrap();
//! ```
//!
//! # See also
//!
//! - [`rsync_core::client`] integrates these helpers for local filesystem
//!   copies.
//! - [`filetime`] for lower-level timestamp manipulation utilities.

use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use filetime::{FileTime, set_file_times, set_symlink_file_times};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[cfg(unix)]
use rustix::{
    fs::{self as unix_fs, AtFlags, CWD, Gid, Uid},
    process::{RawGid, RawUid},
};

#[cfg(unix)]
mod ownership {
    #![allow(unsafe_code)]

    use super::{Gid, RawGid, RawUid, Uid};

    pub(super) fn uid_from_raw(raw: RawUid) -> Uid {
        unsafe { Uid::from_raw(raw) }
    }

    pub(super) fn gid_from_raw(raw: RawGid) -> Gid {
        unsafe { Gid::from_raw(raw) }
    }
}

/// Error produced when metadata preservation fails.
#[derive(Debug)]
pub struct MetadataError {
    context: &'static str,
    path: PathBuf,
    source: io::Error,
}

impl MetadataError {
    /// Creates a new [`MetadataError`] from the supplied context, path, and source error.
    fn new(context: &'static str, path: &Path, source: io::Error) -> Self {
        Self {
            context,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Returns the operation being performed when the error occurred.
    #[must_use]
    pub const fn context(&self) -> &'static str {
        self.context
    }

    /// Returns the path involved in the failing operation.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the underlying [`io::Error`] that triggered this failure.
    #[must_use]
    pub fn source_error(&self) -> &io::Error {
        &self.source
    }

    /// Consumes the error and returns its constituent parts.
    #[must_use]
    pub fn into_parts(self) -> (&'static str, PathBuf, io::Error) {
        (self.context, self.path, self.source)
    }
}

impl fmt::Display for MetadataError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed to {} '{}': {}",
            self.context,
            self.path.display(),
            self.source
        )
    }
}

impl std::error::Error for MetadataError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Applies metadata from `metadata` to the destination directory.
///
/// The helper preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps.
///
/// # Errors
///
/// Returns [`MetadataError`] when filesystem operations fail.
///
/// # Examples
///
/// ```
/// use rsync_meta::{apply_directory_metadata, MetadataError};
/// use std::fs;
/// use tempfile::tempdir;
///
/// # fn demo() -> Result<(), MetadataError> {
/// let temp = tempdir().expect("tempdir");
/// let source = temp.path().join("src-dir");
/// let dest = temp.path().join("dst-dir");
/// fs::create_dir(&source).expect("create source");
/// fs::create_dir(&dest).expect("create dest");
/// let metadata = fs::metadata(&source).expect("source metadata");
/// apply_directory_metadata(&dest, &metadata)?;
/// Ok(())
/// # }
/// # demo().unwrap();
/// ```
pub fn apply_directory_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_directory_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies metadata from `metadata` to the destination directory using explicit options.
pub fn apply_directory_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, true, options)?;
    if options.permissions() {
        set_permissions_like(metadata, destination)?;
    }
    if options.times() {
        set_timestamp_like(metadata, destination, true)?;
    }
    Ok(())
}

/// Applies metadata from `metadata` to the destination file.
///
/// The helper preserves permission bits (best-effort on non-Unix targets) and
/// nanosecond timestamps.
pub fn apply_file_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_file_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies file metadata using explicit [`MetadataOptions`].
pub fn apply_file_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, true, options)?;
    if options.permissions() {
        set_permissions_like(metadata, destination)?;
    }
    if options.times() {
        set_timestamp_like(metadata, destination, true)?;
    }
    Ok(())
}

/// Applies metadata from `metadata` to the destination symbolic link without
/// following the link target.
pub fn apply_symlink_metadata(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    apply_symlink_metadata_with_options(destination, metadata, MetadataOptions::default())
}

/// Applies symbolic link metadata using explicit [`MetadataOptions`].
pub fn apply_symlink_metadata_with_options(
    destination: &Path,
    metadata: &fs::Metadata,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    set_owner_like(metadata, destination, false, options)?;
    if options.times() {
        set_timestamp_like(metadata, destination, false)?;
    }
    Ok(())
}

/// Creates a FIFO at `destination` so metadata can be applied afterwards.
///
/// The function mirrors upstream rsync behaviour by using the source
/// permissions as the mode during creation before delegating to
/// [`apply_file_metadata`] for the final permission and timestamp state.
///
/// # Errors
///
/// Returns [`MetadataError`] if the FIFO cannot be created. This typically
/// occurs when the underlying filesystem does not support FIFOs or the process
/// lacks the required permissions.
///
/// # Examples
///
/// ```
/// use rsync_meta::{create_fifo, apply_file_metadata, MetadataError};
/// use std::fs;
/// #[cfg(unix)]
/// use std::os::unix::fs::FileTypeExt;
/// use tempfile::tempdir;
///
/// # fn demo() -> Result<(), MetadataError> {
/// let temp = tempdir().expect("tempdir");
/// let source_dir = temp.path().join("source");
/// let dest_dir = temp.path().join("dest");
/// fs::create_dir_all(&source_dir).expect("create source");
/// fs::create_dir_all(&dest_dir).expect("create dest");
/// let source_fifo = source_dir.join("pipe");
/// # #[cfg(unix)] {
/// rustix::fs::mknodat(
///     rustix::fs::CWD,
///     &source_fifo,
///     rustix::fs::FileType::Fifo,
///     rustix::fs::Mode::from_bits_truncate(0o640),
///     rustix::fs::makedev(0, 0),
/// )
/// .expect("mkfifo");
/// let metadata = fs::symlink_metadata(&source_fifo).expect("fifo metadata");
/// let dest_fifo = dest_dir.join("pipe");
/// create_fifo(&dest_fifo, &metadata)?;
/// apply_file_metadata(&dest_fifo, &metadata)?;
/// assert!(fs::symlink_metadata(&dest_fifo).expect("dest metadata").file_type().is_fifo());
/// # }
/// # Ok(())
/// # }
/// # demo().unwrap();
/// ```
pub fn create_fifo(destination: &Path, metadata: &fs::Metadata) -> Result<(), MetadataError> {
    create_fifo_inner(destination, metadata)
}

/// Creates a device node at `destination` that mirrors the supplied metadata.
///
/// The helper reconstructs the original major and minor device numbers when
/// running on Unix platforms. Non-Unix platforms report an error indicating
/// that device node creation is unsupported.
pub fn create_device_node(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    create_device_node_inner(destination, metadata)
}

#[cfg(unix)]
fn create_fifo_inner(destination: &Path, metadata: &fs::Metadata) -> Result<(), MetadataError> {
    use rustix::fs::{CWD, FileType, Mode, makedev, mknodat};
    use std::os::unix::fs::PermissionsExt;

    let mode = Mode::from_bits_truncate(metadata.permissions().mode());
    mknodat(CWD, destination, FileType::Fifo, mode, makedev(0, 0))
        .map_err(|error| MetadataError::new("create fifo", destination, io::Error::from(error)))?;
    Ok(())
}

#[cfg(unix)]
fn create_device_node_inner(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    use rustix::fs::{CWD, FileType, Mode, major, makedev, minor, mknodat};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let file_type = metadata.file_type();
    let node_type = if file_type.is_char_device() {
        FileType::CharacterDevice
    } else if file_type.is_block_device() {
        FileType::BlockDevice
    } else {
        return Err(MetadataError::new(
            "create device",
            destination,
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "metadata does not describe a device node",
            ),
        ));
    };

    let mode_bits = metadata.permissions().mode() & 0o777;
    let mode = Mode::from_bits_truncate(mode_bits);
    let raw = metadata.rdev();
    let device = makedev(major(raw), minor(raw));

    mknodat(CWD, destination, node_type, mode, device).map_err(|error| {
        MetadataError::new("create device", destination, io::Error::from(error))
    })?;

    Ok(())
}

#[cfg(not(unix))]
fn create_fifo_inner(destination: &Path, _metadata: &fs::Metadata) -> Result<(), MetadataError> {
    Err(MetadataError::new(
        "create fifo",
        destination,
        io::Error::new(
            io::ErrorKind::Unsupported,
            "FIFO creation is not supported on this platform",
        ),
    ))
}

#[cfg(not(unix))]
fn create_device_node_inner(
    destination: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    Err(MetadataError::new(
        "create device",
        destination,
        io::Error::new(
            io::ErrorKind::Unsupported,
            "device node creation is not supported on this platform",
        ),
    ))
}

/// Options that control metadata preservation during copy operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MetadataOptions {
    preserve_owner: bool,
    preserve_group: bool,
    preserve_permissions: bool,
    preserve_times: bool,
}

impl MetadataOptions {
    /// Creates a new [`MetadataOptions`] value with defaults applied.
    ///
    /// By default the options preserve permissions and timestamps while leaving
    /// ownership disabled so callers can opt-in as needed.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            preserve_owner: false,
            preserve_group: false,
            preserve_permissions: true,
            preserve_times: true,
        }
    }

    /// Requests that ownership be preserved when applying metadata.
    #[must_use]
    pub const fn preserve_owner(mut self, preserve: bool) -> Self {
        self.preserve_owner = preserve;
        self
    }

    /// Requests that the group be preserved when applying metadata.
    #[must_use]
    pub const fn preserve_group(mut self, preserve: bool) -> Self {
        self.preserve_group = preserve;
        self
    }

    /// Requests that permissions be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--perms")]
    pub const fn preserve_permissions(mut self, preserve: bool) -> Self {
        self.preserve_permissions = preserve;
        self
    }

    /// Requests that timestamps be preserved when applying metadata.
    #[must_use]
    #[doc(alias = "--times")]
    pub const fn preserve_times(mut self, preserve: bool) -> Self {
        self.preserve_times = preserve;
        self
    }

    /// Reports whether ownership should be preserved.
    #[must_use]
    pub const fn owner(&self) -> bool {
        self.preserve_owner
    }

    /// Reports whether the group should be preserved.
    #[must_use]
    pub const fn group(&self) -> bool {
        self.preserve_group
    }

    /// Reports whether permissions should be preserved.
    #[must_use]
    pub const fn permissions(&self) -> bool {
        self.preserve_permissions
    }

    /// Reports whether timestamps should be preserved.
    #[must_use]
    pub const fn times(&self) -> bool {
        self.preserve_times
    }
}

impl Default for MetadataOptions {
    fn default() -> Self {
        Self::new()
    }
}

fn set_owner_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
    options: MetadataOptions,
) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        if !options.owner() && !options.group() {
            return Ok(());
        }

        let owner = options
            .owner()
            .then(|| ownership::uid_from_raw(metadata.uid() as RawUid));
        let group = options
            .group()
            .then(|| ownership::gid_from_raw(metadata.gid() as RawGid));

        if owner.is_none() && group.is_none() {
            return Ok(());
        }

        let flags = if follow_symlinks {
            AtFlags::empty()
        } else {
            AtFlags::SYMLINK_NOFOLLOW
        };

        unix_fs::chownat(CWD, destination, owner, group, flags).map_err(|error| {
            MetadataError::new("preserve ownership", destination, io::Error::from(error))
        })?
    }

    #[cfg(not(unix))]
    {
        if options.owner() || options.group() {
            return Err(MetadataError::new(
                "preserve ownership",
                destination,
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "preserving ownership is not supported on this platform",
                ),
            ));
        }
    }

    Ok(())
}

fn set_permissions_like(metadata: &fs::Metadata, destination: &Path) -> Result<(), MetadataError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = metadata.permissions().mode();
        let permissions = PermissionsExt::from_mode(mode);
        fs::set_permissions(destination, permissions)
            .map_err(|error| MetadataError::new("preserve permissions", destination, error))?
    }

    #[cfg(not(unix))]
    {
        let readonly = metadata.permissions().readonly();
        let mut destination_permissions = fs::metadata(destination)
            .map_err(|error| {
                MetadataError::new("inspect destination permissions", destination, error)
            })?
            .permissions();
        destination_permissions.set_readonly(readonly);
        fs::set_permissions(destination, destination_permissions)
            .map_err(|error| MetadataError::new("preserve permissions", destination, error))?
    }

    Ok(())
}

fn set_timestamp_like(
    metadata: &fs::Metadata,
    destination: &Path,
    follow_symlinks: bool,
) -> Result<(), MetadataError> {
    let accessed = FileTime::from_last_access_time(metadata);
    let modified = FileTime::from_last_modification_time(metadata);

    if follow_symlinks {
        set_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?
    } else {
        set_symlink_file_times(destination, accessed, modified)
            .map_err(|error| MetadataError::new("preserve timestamps", destination, error))?
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[cfg(unix)]
    fn current_mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;

        fs::metadata(path).expect("metadata").permissions().mode()
    }

    #[test]
    fn file_permissions_and_times_are_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source.txt");
        let dest = temp.path().join("dest.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, PermissionsExt::from_mode(0o640))
                .expect("set source perms");
        }

        let atime = FileTime::from_unix_time(1_700_000_000, 111_000_000);
        let mtime = FileTime::from_unix_time(1_700_000_100, 222_000_000);
        set_file_times(&source, atime, mtime).expect("set source times");

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata(&dest, &metadata).expect("apply file metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_atime = FileTime::from_last_access_time(&dest_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);

        #[cfg(unix)]
        {
            assert_eq!(current_mode(&dest) & 0o777, 0o640);
        }
    }

    #[cfg(unix)]
    #[test]
    fn file_ownership_is_preserved_when_requested() {
        use rustix::fs::{AtFlags, chownat};

        if rustix::process::geteuid().as_raw() != 0 {
            return;
        }

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-owner.txt");
        let dest = temp.path().join("dest-owner.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let owner = 12_345;
        let group = 54_321;
        chownat(
            CWD,
            &source,
            Some(ownership::uid_from_raw(owner)),
            Some(ownership::gid_from_raw(group)),
            AtFlags::empty(),
        )
        .expect("assign ownership");

        let metadata = fs::metadata(&source).expect("metadata");
        apply_file_metadata_with_options(
            &dest,
            &metadata,
            MetadataOptions::new()
                .preserve_owner(true)
                .preserve_group(true),
        )
        .expect("preserve metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        assert_eq!(dest_meta.uid(), owner);
        assert_eq!(dest_meta.gid(), group);
    }

    #[cfg(unix)]
    #[test]
    fn file_permissions_respect_toggle() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-perms.txt");
        let dest = temp.path().join("dest-perms.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        fs::set_permissions(&source, PermissionsExt::from_mode(0o750)).expect("set source perms");
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            MetadataOptions::new().preserve_permissions(false),
        )
        .expect("apply metadata");

        let mode = current_mode(&dest) & 0o777;
        assert_ne!(mode, 0o750);
    }

    #[test]
    fn file_times_respect_toggle() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-times.txt");
        let dest = temp.path().join("dest-times.txt");
        fs::write(&source, b"data").expect("write source");
        fs::write(&dest, b"data").expect("write dest");

        let atime = FileTime::from_unix_time(1_700_050_000, 100_000_000);
        let mtime = FileTime::from_unix_time(1_700_060_000, 200_000_000);
        set_file_times(&source, atime, mtime).expect("set source times");
        let metadata = fs::metadata(&source).expect("metadata");

        apply_file_metadata_with_options(
            &dest,
            &metadata,
            MetadataOptions::new().preserve_times(false),
        )
        .expect("apply metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_ne!(dest_mtime, mtime);
    }

    #[test]
    fn directory_permissions_and_times_are_preserved() {
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("source-dir");
        let dest = temp.path().join("dest-dir");
        fs::create_dir(&source).expect("create source dir");
        fs::create_dir(&dest).expect("create dest dir");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&source, PermissionsExt::from_mode(0o751))
                .expect("set source perms");
        }

        let atime = FileTime::from_unix_time(1_700_010_000, 0);
        let mtime = FileTime::from_unix_time(1_700_020_000, 333_000_000);
        set_file_times(&source, atime, mtime).expect("set source times");

        let metadata = fs::metadata(&source).expect("metadata");
        apply_directory_metadata(&dest, &metadata).expect("apply dir metadata");

        let dest_meta = fs::metadata(&dest).expect("dest metadata");
        let dest_atime = FileTime::from_last_access_time(&dest_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);

        #[cfg(unix)]
        {
            assert_eq!(current_mode(&dest) & 0o777, 0o751);
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlink_times_are_preserved_without_following_target() {
        use std::os::unix::fs::symlink;

        let temp = tempdir().expect("tempdir");
        let target = temp.path().join("target.txt");
        fs::write(&target, b"data").expect("write target");

        let source_link = temp.path().join("source-link");
        let dest_link = temp.path().join("dest-link");
        symlink(&target, &source_link).expect("create source link");
        symlink(&target, &dest_link).expect("create dest link");

        let atime = FileTime::from_unix_time(1_700_030_000, 444_000_000);
        let mtime = FileTime::from_unix_time(1_700_040_000, 555_000_000);
        set_symlink_file_times(&source_link, atime, mtime).expect("set link times");

        let metadata = fs::symlink_metadata(&source_link).expect("metadata");
        apply_symlink_metadata(&dest_link, &metadata).expect("apply symlink metadata");

        let dest_meta = fs::symlink_metadata(&dest_link).expect("dest metadata");
        let dest_atime = FileTime::from_last_access_time(&dest_meta);
        let dest_mtime = FileTime::from_last_modification_time(&dest_meta);
        assert_eq!(dest_atime, atime);
        assert_eq!(dest_mtime, mtime);

        let dest_target = fs::read_link(&dest_link).expect("read dest link");
        assert_eq!(dest_target, target);
    }
}
