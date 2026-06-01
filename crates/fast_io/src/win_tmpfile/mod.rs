//! Delete-on-close temporary file creation for Windows.
//!
//! This module provides both low-level Win32 wrappers ([`open_delete_on_close_tmpfile`],
//! [`set_delete_on_close`], [`clear_delete_on_close`], [`commit_delete_on_close`]) and
//! a higher-level [`WindowsTempFile`] guard type that owns the delete-on-close
//! handle and exposes a safe `commit_to` finalization method.
//!
//! The [`open_win_temp_file`] convenience function probes the filesystem once
//! and returns either a [`WinTempFileResult::DeleteOnClose`] or
//! [`WinTempFileResult::Unavailable`], letting callers fall back to named temp
//! files without error handling.
//!
//! # Comparison with `O_TMPFILE`
//!
//! | Property | Linux `O_TMPFILE` | Windows `FileDispositionInfo` |
//! |---|---|---|
//! | Directory entry | None until `linkat` | Visible with unique name |
//! | Crash cleanup | Kernel reclaims inode | Kernel deletes file on close |
//! | Commit mechanism | `linkat(2)` | Clear disposition + `MoveFileExW` |
//! | Filesystem support | ext4, xfs, btrfs, tmpfs | NTFS, ReFS, FAT32 |
//!
//! Both provide the same semantic guarantee: if the process crashes before
//! commit, no orphaned partial file remains at the destination.

mod low_level;
mod types;

pub use low_level::{
    clear_delete_on_close, commit_delete_on_close, delete_on_close_available,
    open_delete_on_close_tmpfile, rename_temp_to_dest, set_delete_on_close,
};
pub use types::{
    WinDeleteOnCloseSupport, WinTempFileResult, WindowsTempFile, open_win_temp_file,
    win_tmpfile_probe,
};
