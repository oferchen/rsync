//! File entry construction and metadata collection for the generator role.
//!
//! Builds `FileEntry` values from filesystem metadata, handling platform-specific
//! fields (mode, uid/gid, devices, symlinks, hardlink dev/ino).
//!
//! # Submodules
//!
//! - `create` - `create_entry` classification and metadata population
//! - `fake_super` - `user.rsync.%stat` xattr override mapping
//! - `device` - rdev major/minor extraction
//! - `munge` - symlink-target munge prefix stripping
//!
//! # Upstream Reference
//!
//! - `flist.c:make_file()` - determines file type and populates `file_struct`
//! - `flist.c:readlink_stat()` - symlink target resolution
//! - `xattrs.c:get_stat_xattr()` - fake-super override applied via
//!   `x_lstat()` before `make_file()` consumes the stat values

mod create;
mod device;
mod fake_super;
mod munge;

#[cfg(all(unix, test))]
pub(in crate::generator) use self::device::rdev_to_major_minor;
