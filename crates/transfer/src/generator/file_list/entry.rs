//! File entry construction and metadata collection for the generator role.
//!
//! Builds `FileEntry` values from filesystem metadata, handling platform-specific
//! fields (mode, uid/gid, devices, symlinks, hardlink dev/ino).
//!
//! # Upstream Reference
//!
//! - `flist.c:make_file()` - determines file type and populates `file_struct`
//! - `flist.c:readlink_stat()` - symlink target resolution

use std::io;
use std::path::{Path, PathBuf};

use protocol::flist::FileEntry;

use super::super::GeneratorContext;

impl GeneratorContext {
    /// Creates a `FileEntry` from filesystem metadata for wire transmission.
    ///
    /// Populates mode, mtime, uid/gid, atime/crtime, symlink targets, device numbers,
    /// and hardlink dev/ino fields based on the active preservation flags.
    ///
    /// # Upstream Reference
    ///
    /// - `flist.c:make_file()` - determines file type and populates the `file_struct`
    /// - Device files (block/char) use `new_block_device`/`new_char_device` with rdev fields
    /// - Special files (FIFOs/sockets) use `new_fifo`/`new_socket`
    pub(in crate::generator) fn create_entry(
        &self,
        full_path: &Path,
        relative_path: PathBuf,
        metadata: &std::fs::Metadata,
    ) -> io::Result<FileEntry> {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;

        let file_type = metadata.file_type();

        let mut entry = if file_type.is_file() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = if metadata.permissions().readonly() {
                0o444
            } else {
                0o644
            };

            FileEntry::new_file(relative_path, metadata.len(), mode)
        } else if file_type.is_dir() {
            #[cfg(unix)]
            let mode = metadata.mode() & 0o7777;
            #[cfg(not(unix))]
            let mode = 0o755;

            FileEntry::new_directory(relative_path, mode)
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(full_path).unwrap_or_else(|_| PathBuf::from(""));

            FileEntry::new_symlink(relative_path, target)
        } else {
            // Device and special file types (Unix only)
            #[cfg(unix)]
            {
                use std::os::unix::fs::FileTypeExt;
                let mode = metadata.mode() & 0o7777;
                if file_type.is_block_device() {
                    let (major, minor) = rdev_to_major_minor(metadata.rdev());
                    FileEntry::new_block_device(relative_path, mode, major, minor)
                } else if file_type.is_char_device() {
                    let (major, minor) = rdev_to_major_minor(metadata.rdev());
                    FileEntry::new_char_device(relative_path, mode, major, minor)
                } else if file_type.is_fifo() {
                    FileEntry::new_fifo(relative_path, mode)
                } else if file_type.is_socket() {
                    FileEntry::new_socket(relative_path, mode)
                } else {
                    FileEntry::new_file(relative_path, 0, 0o644)
                }
            }
            #[cfg(not(unix))]
            {
                FileEntry::new_file(relative_path, 0, 0o644)
            }
        };

        // upstream: flist.c:make_file() - set mtime
        #[cfg(unix)]
        {
            entry.set_mtime(metadata.mtime(), metadata.mtime_nsec() as u32);
        }
        #[cfg(not(unix))]
        {
            if let Ok(mtime) = metadata.modified() {
                if let Ok(duration) = mtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_mtime(duration.as_secs() as i64, duration.subsec_nanos());
                }
            }
        }

        // Set access time if preserving (upstream: flist.c:489-494)
        #[cfg(unix)]
        if self.config.flags.atimes && !entry.is_dir() {
            entry.set_atime(metadata.atime());
        }
        #[cfg(not(unix))]
        if self.config.flags.atimes && !entry.is_dir() {
            if let Ok(atime) = metadata.accessed() {
                if let Ok(duration) = atime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_atime(duration.as_secs() as i64);
                }
            }
        }

        // Set creation time if preserving (upstream: flist.c:495-498)
        if self.config.flags.crtimes {
            if let Ok(crtime) = metadata.created() {
                if let Ok(duration) = crtime.duration_since(std::time::UNIX_EPOCH) {
                    entry.set_crtime(duration.as_secs() as i64);
                }
            }
        }

        // upstream: flist.c:make_file() - set uid/gid
        #[cfg(unix)]
        if self.config.flags.owner {
            entry.set_uid(metadata.uid());
        }
        #[cfg(unix)]
        if self.config.flags.group {
            entry.set_gid(metadata.gid());
        }

        // Store dev/ino for hardlink detection (post-sort assignment).
        // upstream: flist.c:make_file() stores tmp_dev/tmp_ino when preserve_hard_links
        #[cfg(unix)]
        if self.config.flags.hard_links && metadata.nlink() > 1 && !metadata.is_dir() {
            entry.set_hardlink_dev(metadata.dev() as i64);
            entry.set_hardlink_ino(metadata.ino() as i64);
        }

        Ok(entry)
    }
}

/// Extracts major and minor device numbers from a raw `rdev` value.
///
/// The layout differs by platform:
/// - **Linux**: Split encoding where major/minor span non-contiguous bits.
/// - **macOS/BSD**: Major in high byte, minor in low 24 bits.
///
/// # Upstream Reference
///
/// Mirrors glibc `major()`/`minor()` macros used by upstream rsync to populate
/// `rdev_major`/`rdev_minor` in `file_struct`.
#[cfg(all(unix, target_os = "linux"))]
pub(in crate::generator) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = ((rdev >> 8) & 0xfff) as u32 | (((rdev >> 32) & !0xfff) as u32);
    let minor = (rdev & 0xff) as u32 | (((rdev >> 12) & !0xff) as u32);
    (major, minor)
}

/// Extracts major and minor device numbers from a raw `rdev` value (BSD/macOS).
///
/// BSD layout: major in bits 31-24, minor in bits 23-0.
#[cfg(all(unix, not(target_os = "linux")))]
pub(in crate::generator) fn rdev_to_major_minor(rdev: u64) -> (u32, u32) {
    let major = (rdev >> 24) as u32;
    let minor = (rdev & 0xffffff) as u32;
    (major, minor)
}
