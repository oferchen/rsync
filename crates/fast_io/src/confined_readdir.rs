//! Confined directory enumeration for the source-tree scan.
//!
//! The scan that discovers a directory's entries is a confinement decision in
//! its own right, separate from the open that later reads a file's contents.
//! Enumerating by path lets a parent component raced into a symlink redirect
//! the whole listing, so names from outside the transfer root enter the file
//! list and are then copied out - the escape
//! `sender-scan-dir-escape` catches.
//!
//! Resolving the directory and then calling `read_dir` on the same path does
//! not close it: the parent can flip between the two, and the second lookup
//! walks the new target. The directory has to be *opened* through the confined
//! walk and enumerated from that descriptor, so the names come from the
//! directory the walk adjudicated and nothing else.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/flist.c` `send_directory()` enumerates the descriptor its
//!   confined open produced, not the path it was asked for.
//! - `rsync-3.5.0/syscall.c:2891` `ds_descend()` - the per-component walk this
//!   anchors on, shared with the content open.

use std::ffi::OsString;
use std::io;
use std::path::Path;

use crate::confined_open::validate_relative;

/// Lists the names in `root`/`relative`, resolving every component of
/// `relative` beneath `root` and reading the entries from the resulting
/// descriptor.
///
/// `.` and `..` are omitted, matching what a caller building child paths
/// needs and what upstream's scan skips. Order is whatever the filesystem
/// returns; callers that need determinism sort, exactly as they do for
/// `fs::read_dir`.
///
/// # Errors
///
/// Propagates the walk's refusal (an absolute symlink target, a `..` above
/// the anchor, an excluded component) so a refused scan is never mistaken for
/// an empty directory. Returns [`io::ErrorKind::Unsupported`] on non-Unix,
/// where there is no dirfd model to anchor on.
pub fn read_dir_confined(root: &Path, relative: &Path) -> io::Result<Vec<OsString>> {
    validate_relative(relative)?;
    imp::read_dir_confined(root, relative)
}

#[cfg(unix)]
mod imp {
    use super::*;

    use std::ffi::CStr;
    use std::os::fd::IntoRawFd;
    use std::os::unix::ffi::OsStrExt;

    use crate::dir_sandbox::{ConfinePolicy, DirSandbox};

    pub(super) fn read_dir_confined(root: &Path, relative: &Path) -> io::Result<Vec<OsString>> {
        // The same walk the content open uses: the root is operator-trusted
        // and opened plainly, every component of `relative` is adjudicated.
        let sandbox = DirSandbox::open_dest_anchor_confined(
            root,
            relative,
            ConfinePolicy::operator_trusted(),
        )?;
        read_names(&sandbox)
    }

    /// Enumerates the sandbox's resolved directory from its descriptor.
    ///
    /// `fdopendir` takes ownership of the descriptor it is handed and
    /// `closedir` releases it, while the sandbox keeps its own for the
    /// caller's later use - so this hands over a duplicate, never the
    /// sandbox's.
    #[allow(unsafe_code)]
    fn read_names(sandbox: &DirSandbox) -> io::Result<Vec<OsString>> {
        let duplicate = sandbox.root_dirfd().try_clone_to_owned()?;
        let raw = duplicate.into_raw_fd();

        // SAFETY: `raw` is an open directory descriptor this function solely
        // owns, freshly duplicated above and not shared with the sandbox.
        // `fdopendir` takes that ownership on success; on failure it does not,
        // which is why the error arm closes `raw` itself.
        let dir = unsafe { libc::fdopendir(raw) };
        if dir.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: `fdopendir` failed, so ownership of `raw` never
            // transferred and this is the only close of it.
            unsafe { libc::close(raw) };
            return Err(error);
        }

        let mut names = Vec::new();
        loop {
            // SAFETY: `dir` is the live `DIR*` returned above, not yet closed.
            let entry = unsafe { libc::readdir(dir) };
            if entry.is_null() {
                // A NULL return is end-of-stream. `readdir` also reports an
                // error this way, distinguished only by `errno`; upstream's
                // scan loop (`while ((di = readdir(d)) != NULL)`,
                // `flist.c` `send_directory()`) treats NULL as the end, and
                // this mirrors that rather than inventing a stricter rule.
                break;
            }
            // SAFETY: `entry` points at a `dirent` owned by `dir` and valid
            // until the next `readdir`/`closedir`; `d_name` is a NUL-
            // terminated array within it, copied out before either happens.
            let name = unsafe { CStr::from_ptr(std::ptr::addr_of!((*entry).d_name).cast()) };
            let name = name.to_bytes();
            if name == b"." || name == b".." {
                continue;
            }
            names.push(std::ffi::OsStr::from_bytes(name).to_owned());
        }

        // SAFETY: `dir` is live and this is its only close; the descriptor
        // `fdopendir` took ownership of is released with it.
        unsafe { libc::closedir(dir) };
        Ok(names)
    }
}

#[cfg(not(unix))]
mod imp {
    use super::*;

    pub(super) fn read_dir_confined(_root: &Path, _relative: &Path) -> io::Result<Vec<OsString>> {
        // No dirfd model to anchor on, matching the platform limitation the
        // sibling confined open already documents.
        Err(io::Error::from(io::ErrorKind::Unsupported))
    }
}
