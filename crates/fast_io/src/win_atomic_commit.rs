//! No-follow atomic file commit primitives for the Windows receiver.
//!
//! These mirror, on Windows, the Unix receiver's reparse-point TOCTOU
//! hardening (the residual half of CVE-2024-12747). On Unix the receiver
//! creates its temp file with `openat(dirfd, leaf, O_CREAT | O_EXCL |
//! O_NOFOLLOW)` and commits it with `renameat(dirfd, leaf, dirfd, leaf)`, both
//! resolved against a directory file descriptor pinned at receiver setup (see
//! [`crate::dir_sandbox`]). That anchoring means an attacker who swaps a path
//! component for a symlink/junction/mount-point between the check and the use
//! cannot redirect the write or the final rename outside the destination tree.
//!
//! Windows has no `openat`/`renameat`, but the same guarantee is available via:
//!
//! - [`create_new_no_follow`] - `CreateFileW`-equivalent open with
//!   `CREATE_NEW` + `FILE_FLAG_OPEN_REPARSE_POINT`, so a reparse point planted
//!   at the temp leaf is opened as the reparse point itself (and `CREATE_NEW`
//!   then fails) rather than traversed. This is the analog of `O_EXCL |
//!   O_NOFOLLOW`.
//! - [`rename_no_follow`] - a handle-based commit rename. The destination
//!   parent is opened and validated as a real directory (not a reparse point),
//!   and the rename is issued through
//!   `SetFileInformationByHandle(FileRenameInfo)` with `FILE_RENAME_INFO`'s
//!   `RootDirectory` set to that validated directory handle, so the new name
//!   resolves relative to the pinned handle instead of by re-walking the path.
//!   This is the analog of `renameat` anchored on a dirfd.
//!
//! All Win32 FFI lives here in `fast_io` (a permitted-unsafe crate); the
//! `transfer` receiver calls the safe functions.

#[cfg(windows)]
mod imp {
    use std::ffi::OsStr;
    use std::fs::{File, OpenOptions};
    use std::io;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::fs::OpenOptionsExt;
    use std::os::windows::io::AsRawHandle;
    use std::path::Path;

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ,
        FILE_GENERIC_WRITE, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
        FileRenameInfo, GetFileInformationByHandle, SetFileInformationByHandle,
    };

    /// Shared-access mask allowing concurrent readers, writers, and deleters so
    /// the handle-based rename works while other handles (e.g. antivirus) are
    /// open. Mirrors upstream's tolerance for external readers.
    const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;

    /// Atomically creates `path` with `CREATE_NEW` semantics without following a
    /// reparse point at the final component - the Windows analog of `O_CREAT |
    /// O_EXCL | O_NOFOLLOW`.
    ///
    /// `FILE_FLAG_OPEN_REPARSE_POINT` makes a symlink/junction pre-planted at
    /// `path` open as the reparse point itself rather than being traversed, so
    /// `CREATE_NEW` fails with `ERROR_FILE_EXISTS` (surfaced as
    /// [`io::ErrorKind::AlreadyExists`], which the receiver's temp-name loop
    /// retries) instead of creating the file through the attacker-controlled
    /// link. The handle is granted `DELETE` access and shared for delete so the
    /// later [`rename_no_follow`] commit can rename it by handle.
    ///
    /// # Errors
    ///
    /// Propagates the underlying open failure. A pre-existing name yields
    /// [`io::ErrorKind::AlreadyExists`]; a missing parent yields
    /// [`io::ErrorKind::NotFound`].
    pub fn create_new_no_follow(path: &Path) -> io::Result<File> {
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .access_mode(FILE_GENERIC_READ | FILE_GENERIC_WRITE | DELETE)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }

    /// Commits `temp_path` to `dest_path` with a handle-anchored rename that a
    /// concurrent reparse-point swap on the destination parent cannot redirect.
    ///
    /// Steps (mirroring the Unix `renameat` anchoring):
    ///
    /// 1. Open `temp_path` with `FILE_FLAG_OPEN_REPARSE_POINT` + `DELETE`
    ///    access and reject it if it resolved to a reparse point - closing a
    ///    swap of the temp leaf on the source side.
    /// 2. Open `dest_path`'s parent directory with `FILE_FLAG_BACKUP_SEMANTICS
    ///    | FILE_FLAG_OPEN_REPARSE_POINT` and reject it unless it is a real
    ///    directory (not a reparse point) - closing a junction/mount-point swap
    ///    on the commit parent.
    /// 3. Rename the temp handle via `SetFileInformationByHandle(FileRenameInfo)`
    ///    with `RootDirectory` set to the validated directory handle and
    ///    `FileName` the single destination leaf, so the target name resolves
    ///    relative to the pinned handle rather than by re-walking the path.
    ///
    /// `replace_existing` maps to `FILE_RENAME_INFO::ReplaceIfExists` (upstream
    /// `do_rename` overwrites the destination).
    ///
    /// # Errors
    ///
    /// - [`io::ErrorKind::InvalidInput`] if `dest_path` lacks a parent or file
    ///   name.
    /// - An error whose `raw_os_error()` is `ERROR_NOT_SAME_DEVICE` (17) when
    ///   the temp file is on another volume; callers fall back to copy+remove
    ///   (upstream `util1.c:robust_rename()`).
    /// - Any underlying open, validation, or rename failure. A reparse-point
    ///   swap detected in step 1 or 2 surfaces as an error, so the commit fails
    ///   safe rather than following the redirect.
    pub fn rename_no_follow(
        temp_path: &Path,
        dest_path: &Path,
        replace_existing: bool,
    ) -> io::Result<()> {
        let dest_dir = dest_path.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination path has no parent directory",
            )
        })?;
        let leaf = dest_path.file_name().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination path has no file name",
            )
        })?;

        // (1) Source handle: no-follow open with DELETE access (required by
        // FileRenameInfo). Reject a reparse point at the temp leaf.
        let src = OpenOptions::new()
            .access_mode(DELETE | FILE_GENERIC_READ)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(temp_path)?;
        if file_attributes(&src)? & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to commit: temp file resolved to a reparse point",
            ));
        }

        // (2) Destination parent handle: no-follow open (BACKUP_SEMANTICS is
        // required to obtain a directory handle). Reject a reparse point so a
        // junction swap on the parent cannot redirect the rename, and confirm
        // it is a directory.
        let dir = OpenOptions::new()
            .access_mode(FILE_GENERIC_READ)
            .share_mode(SHARE_ALL)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(dest_dir)?;
        let dir_attrs = file_attributes(&dir)?;
        if dir_attrs & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "refusing to commit: destination parent is a reparse point",
            ));
        }
        if dir_attrs & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "destination parent is not a directory",
            ));
        }

        // (3) Handle-anchored rename relative to the validated directory handle.
        set_rename_info(&src, dir.as_raw_handle() as HANDLE, leaf, replace_existing)
    }

    /// Returns the file attribute bitmask of an open handle.
    fn file_attributes(file: &File) -> io::Result<u32> {
        // BY_HANDLE_FILE_INFORMATION is a plain-old-data struct (integers and
        // FILETIME fields); a zeroed value is valid before the call fills it.
        // SAFETY: zeroing a POD struct with no invalid bit patterns is sound,
        // and `file` owns a valid, open handle for the duration of the call.
        // `info` is a correctly sized, writable `BY_HANDLE_FILE_INFORMATION`;
        // GetFileInformationByHandle only writes into it and returns 0 on
        // failure.
        #[allow(unsafe_code)]
        unsafe {
            let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
            let ok = GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info);
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(info.dwFileAttributes)
        }
    }

    /// Issues `SetFileInformationByHandle(FileRenameInfo)` on `src`, resolving
    /// `leaf` relative to the `root_dir` directory handle.
    ///
    /// `FILE_RENAME_INFO` is a variable-length struct whose trailing
    /// `FileName[1]` field is a flexible array; the buffer is allocated as a
    /// `Vec<u64>` so it is large enough for the leaf and aligned to the
    /// struct's 8-byte (HANDLE) alignment.
    fn set_rename_info(
        src: &File,
        root_dir: HANDLE,
        leaf: &OsStr,
        replace_existing: bool,
    ) -> io::Result<()> {
        let name: Vec<u16> = leaf.encode_wide().collect();
        let name_bytes = name.len() * size_of::<u16>();
        // size_of::<FILE_RENAME_INFO>() already includes the 2-byte FileName[1]
        // stub, so header + name_bytes slightly over-allocates - harmless.
        let total = size_of::<FILE_RENAME_INFO>() + name_bytes;
        let words = total.div_ceil(size_of::<u64>());
        let mut buf = vec![0u64; words];
        let base = buf.as_mut_ptr().cast::<u8>();

        // SAFETY: `base` points at a zeroed, 8-byte-aligned buffer of at least
        // `total` bytes (Vec<u64> guarantees the alignment FILE_RENAME_INFO's
        // HANDLE field needs). Every field written below lies within `total`,
        // and exactly `name.len()` u16s are copied into the trailing FileName
        // array, whose offset plus length stays within the allocation. `src`
        // holds a valid handle with DELETE access for the SetFileInformation
        // call, which only reads `total` bytes from `base`.
        #[allow(unsafe_code)]
        unsafe {
            let info = base.cast::<FILE_RENAME_INFO>();
            (*info).Anonymous.ReplaceIfExists = replace_existing;
            (*info).RootDirectory = root_dir;
            (*info).FileNameLength = name_bytes as u32;
            let name_dst = std::ptr::addr_of_mut!((*info).FileName).cast::<u16>();
            std::ptr::copy_nonoverlapping(name.as_ptr(), name_dst, name.len());

            let ok = SetFileInformationByHandle(
                src.as_raw_handle() as HANDLE,
                FileRenameInfo,
                base.cast(),
                total as u32,
            );
            if ok == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::io::Write;
        use tempfile::tempdir;

        /// Happy path: an anchored commit into an ordinary directory renames the
        /// temp file to the destination leaf and removes the temp file.
        #[test]
        fn rename_no_follow_commits_into_plain_dir() {
            let dir = tempdir().expect("tempdir");
            let temp = dir.path().join(".payload.AbC123");
            let dest = dir.path().join("payload.bin");
            {
                let mut f = create_new_no_follow(&temp).expect("create temp");
                f.write_all(b"anchored commit").expect("write");
                f.flush().expect("flush");
            }

            rename_no_follow(&temp, &dest, true).expect("anchored rename");

            assert!(!temp.exists(), "temp file must be gone after rename");
            assert_eq!(std::fs::read(&dest).expect("read dest"), b"anchored commit");
        }

        /// `ReplaceIfExists = true` overwrites an existing destination, matching
        /// upstream `do_rename`.
        #[test]
        fn rename_no_follow_replaces_existing() {
            let dir = tempdir().expect("tempdir");
            let dest = dir.path().join("existing.bin");
            std::fs::write(&dest, b"old").expect("seed dest");
            let temp = dir.path().join(".existing.XyZ789");
            {
                let mut f = create_new_no_follow(&temp).expect("create temp");
                f.write_all(b"new").expect("write");
            }

            rename_no_follow(&temp, &dest, true).expect("replace");
            assert_eq!(std::fs::read(&dest).expect("read"), b"new");
        }

        /// `create_new_no_follow` fails with `AlreadyExists` when the name is
        /// taken, preserving the `CREATE_NEW` retry contract of the temp-name
        /// loop.
        #[test]
        fn create_new_no_follow_rejects_existing_name() {
            let dir = tempdir().expect("tempdir");
            let path = dir.path().join(".taken.Aa0000");
            let _first = create_new_no_follow(&path).expect("first create");
            let err = create_new_no_follow(&path).expect_err("second must fail");
            assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        }

        /// Anchoring proof (the CVE-2024-12747 residual): when the destination
        /// parent is swapped for a directory reparse point (junction) pointing
        /// at an attacker-controlled tree, the anchored commit must refuse to
        /// follow it - the attacker directory must never receive the file.
        ///
        /// Junctions are created without privilege via
        /// `create_directory_symlink_or_junction`, so this runs on unprivileged
        /// CI. If even the junction fallback is unavailable the test skips.
        #[test]
        fn rename_no_follow_refuses_reparse_point_parent() {
            let root = tempdir().expect("tempdir");
            let real_dest = root.path().join("real_dest");
            let attacker = root.path().join("attacker");
            std::fs::create_dir(&real_dest).expect("real_dest");
            std::fs::create_dir(&attacker).expect("attacker");
            // A sentinel proving the attacker tree is untouched.
            std::fs::write(attacker.join("keep.txt"), b"keep").expect("sentinel");

            // Temp source lives in the root, outside the swapped directory.
            let temp = root.path().join(".loot.Zz9999");
            {
                let mut f = create_new_no_follow(&temp).expect("create temp");
                f.write_all(b"loot").expect("write");
            }

            // Swap: move the real destination aside and plant a junction at its
            // path pointing at the attacker tree.
            let aside = root.path().join("real_dest.aside");
            std::fs::rename(&real_dest, &aside).expect("move aside");
            match crate::win_symlink::create_directory_symlink_or_junction(&attacker, &real_dest) {
                Ok(_) => {}
                Err(err) => {
                    eprintln!("skipping: cannot create reparse point ({err})");
                    return;
                }
            }

            let dest = real_dest.join("victim.bin");
            let result = rename_no_follow(&temp, &dest, true);

            assert!(
                result.is_err(),
                "anchored rename must refuse a reparse-point destination parent"
            );
            assert!(
                !attacker.join("victim.bin").exists(),
                "attacker tree must never receive the committed file"
            );
            assert!(
                attacker.join("keep.txt").exists(),
                "attacker sentinel must be untouched"
            );
        }
    }
}

#[cfg(windows)]
pub use imp::{create_new_no_follow, rename_no_follow};
