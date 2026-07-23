use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::super::is_fifo;

/// File type captured for [`LocalCopyMetadata`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LocalCopyFileKind {
    /// Regular file entry.
    File,
    /// Directory entry.
    Directory,
    /// Symbolic link entry.
    Symlink,
    /// FIFO entry.
    Fifo,
    /// Character device entry.
    CharDevice,
    /// Block device entry.
    BlockDevice,
    /// Unix domain socket entry.
    Socket,
    /// Unknown or platform specific entry.
    Other,
}

impl LocalCopyFileKind {
    pub(super) fn from_file_type(file_type: fs::FileType) -> Self {
        if file_type.is_dir() {
            return Self::Directory;
        }
        if file_type.is_symlink() {
            return Self::Symlink;
        }
        if file_type.is_file() {
            return Self::File;
        }
        // Sockets and devices must be classified before the `is_fifo` helper,
        // which deliberately treats sockets as FIFOs for CopyFifo routing
        // (support.rs). Distinguishing the kind here keeps `--list-only`
        // permission strings faithful to upstream (`s` for sockets, `c`/`b`
        // for devices) rather than collapsing them to `p`.
        #[cfg(unix)]
        {
            use std::os::unix::fs::FileTypeExt;

            if file_type.is_socket() {
                return Self::Socket;
            }
            if file_type.is_char_device() {
                return Self::CharDevice;
            }
            if file_type.is_block_device() {
                return Self::BlockDevice;
            }
        }
        if is_fifo(file_type) {
            return Self::Fifo;
        }
        Self::Other
    }

    /// Maps a POSIX `st_mode` (including the `S_IFMT` type bits) to the file
    /// kind, returning `None` for a regular file or an unrecognised type.
    ///
    /// Used by the fake-super reporting override so a regular placeholder is
    /// itemized as the virtual type its `user.rsync.%stat` xattr encodes,
    /// while a `%stat` that still describes a regular file leaves the kind
    /// untouched.
    #[cfg(all(unix, feature = "xattr"))]
    fn from_stat_mode(mode: u32) -> Option<Self> {
        match mode & 0o170000 {
            0o040000 => Some(Self::Directory),
            0o120000 => Some(Self::Symlink),
            0o010000 => Some(Self::Fifo),
            0o020000 => Some(Self::CharDevice),
            0o060000 => Some(Self::BlockDevice),
            0o140000 => Some(Self::Socket),
            _ => None,
        }
    }

    /// Returns whether the kind represents a directory.
    #[must_use]
    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Directory)
    }
}

/// Metadata snapshot recorded for events emitted by [`super::LocalCopyRecord`].
#[derive(Clone, Debug)]
pub struct LocalCopyMetadata {
    kind: LocalCopyFileKind,
    len: u64,
    modified: Option<SystemTime>,
    mode: Option<u32>,
    uid: Option<u32>,
    gid: Option<u32>,
    nlink: Option<u64>,
    symlink_target: Option<PathBuf>,
}

impl LocalCopyMetadata {
    pub(in crate::local_copy) fn from_metadata(
        metadata: &fs::Metadata,
        symlink_target: Option<PathBuf>,
    ) -> Self {
        let file_type = metadata.file_type();
        let kind = LocalCopyFileKind::from_file_type(file_type);
        let len = metadata.len();
        let modified = metadata.modified().ok();

        #[cfg(unix)]
        let (mode, uid, gid, nlink) = {
            use std::os::unix::fs::MetadataExt;
            (
                Some(metadata.mode()),
                Some(metadata.uid()),
                Some(metadata.gid()),
                Some(metadata.nlink()),
            )
        };

        #[cfg(not(unix))]
        let (mode, uid, gid, nlink) = (None, None, None, None);

        // upstream: log.c:643-654 - `%L` renders ` -> %s` for symlinks
        // (`F_SYMLINK(file)`) and ` => %s` for hardlink aliases (`hlink`).
        // Both reuse this `symlink_target` slot; keep the caller-supplied
        // reference target for files and special files (devices/FIFOs/sockets)
        // too so a hard-linked device alias can render `hD+++++++++ x => leader`.
        // The CLI placeholder distinguishes ` -> ` from ` => ` by the event kind.
        // Directories never carry a `%L` trailer.
        let target = match kind {
            LocalCopyFileKind::Directory => None,
            _ => symlink_target,
        };

        Self {
            kind,
            len,
            modified,
            mode,
            uid,
            gid,
            nlink,
            symlink_target: target,
        }
    }

    /// Applies the upstream fake-super `st_mode` override for reporting.
    ///
    /// Under `--fake-super`, a device/FIFO/socket is stored on disk as a
    /// regular placeholder file that carries the real mode and device numbers
    /// in its `user.rsync.%stat` xattr. Upstream's sender virtualises the stat
    /// via `get_stat_xattr()` before itemizing, so the placeholder is reported
    /// as its true type. This mirrors that override for the local-copy path:
    /// when `fake_super` is active and this snapshot describes a regular file
    /// whose source `%stat` decodes to a non-regular type, the reported kind
    /// and mode are replaced. The on-disk file and the copied destination stay
    /// regular placeholders; only the itemized type char and mode change.
    ///
    /// # Upstream Reference
    ///
    /// - `xattrs.c:1135 get_stat_xattr()` - overrides `st_mode`/`st_rdev` from
    ///   the `%stat` xattr on the sender when `am_root < 0` (fake-super).
    #[must_use]
    #[cfg_attr(not(all(unix, feature = "xattr")), allow(unused_mut))]
    pub(in crate::local_copy) fn virtualize_fake_super(
        mut self,
        source: &Path,
        fake_super: bool,
    ) -> Self {
        #[cfg(all(unix, feature = "xattr"))]
        {
            if fake_super
                && matches!(self.kind, LocalCopyFileKind::File)
                && let Ok(Some(stat)) = ::metadata::load_fake_super(source)
                && let Some(kind) = LocalCopyFileKind::from_stat_mode(stat.mode)
            {
                self.kind = kind;
                self.mode = Some(stat.mode);
            }
        }
        #[cfg(not(all(unix, feature = "xattr")))]
        {
            let _ = (source, fake_super);
        }
        self
    }

    /// Presents a `--copy-devices` block/char device as a regular file for
    /// reporting, so `--list-only` and itemized output show `-rw-...` with the
    /// device's readable byte length instead of `brw-...`/`crw-...` and `0`.
    ///
    /// Mirrors upstream `flist.c:1451-1460 make_file()`, which rewrites the
    /// device stat to `S_IFREG | (mode & ACCESSPERMS)` with `get_device_size()`
    /// before the entry is ever itemized. A no-op for non-device kinds.
    #[must_use]
    pub(in crate::local_copy) fn virtualize_copy_device_as_file(mut self, size: u64) -> Self {
        if matches!(
            self.kind,
            LocalCopyFileKind::CharDevice | LocalCopyFileKind::BlockDevice
        ) {
            self.kind = LocalCopyFileKind::File;
            self.len = size;
            if let Some(mode) = self.mode {
                // upstream: S_IFREG | (st.st_mode & ACCESSPERMS).
                self.mode = Some(0o100_000 | (mode & 0o7777));
            }
        }
        self
    }

    /// Returns the entry kind associated with the metadata.
    #[must_use]
    pub const fn kind(&self) -> LocalCopyFileKind {
        self.kind
    }

    /// Returns the entry length in bytes.
    #[must_use]
    pub const fn len(&self) -> u64 {
        self.len
    }

    /// Returns whether the metadata describes an empty entry.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Returns the recorded modification time, when available.
    pub const fn modified(&self) -> Option<SystemTime> {
        self.modified
    }

    /// Returns the Unix permission bits when available.
    pub const fn mode(&self) -> Option<u32> {
        self.mode
    }

    /// Returns the numeric owner identifier when available.
    pub const fn uid(&self) -> Option<u32> {
        self.uid
    }

    /// Returns the numeric group identifier when available.
    pub const fn gid(&self) -> Option<u32> {
        self.gid
    }

    /// Returns the hard link count when available.
    pub const fn nlink(&self) -> Option<u64> {
        self.nlink
    }

    /// Returns the recorded symbolic link target when the metadata describes a symlink.
    pub fn symlink_target(&self) -> Option<&Path> {
        self.symlink_target.as_deref()
    }

    /// Consumes the metadata and returns the symlink target path, if present.
    #[must_use]
    pub fn into_symlink_target(self) -> Option<PathBuf> {
        self.symlink_target
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_kind_is_directory_returns_true_for_directory() {
        assert!(LocalCopyFileKind::Directory.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_file() {
        assert!(!LocalCopyFileKind::File.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_symlink() {
        assert!(!LocalCopyFileKind::Symlink.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_fifo() {
        assert!(!LocalCopyFileKind::Fifo.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_char_device() {
        assert!(!LocalCopyFileKind::CharDevice.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_block_device() {
        assert!(!LocalCopyFileKind::BlockDevice.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_socket() {
        assert!(!LocalCopyFileKind::Socket.is_directory());
    }

    #[test]
    fn file_kind_is_directory_returns_false_for_other() {
        assert!(!LocalCopyFileKind::Other.is_directory());
    }

    #[test]
    fn file_kind_clone_produces_equal_value() {
        let kind = LocalCopyFileKind::File;
        let cloned = kind;
        assert_eq!(kind, cloned);
    }

    #[test]
    fn file_kind_copy_produces_equal_value() {
        let kind = LocalCopyFileKind::Directory;
        let copied = kind;
        assert_eq!(kind, copied);
    }

    #[test]
    fn file_kind_debug_format_contains_variant_name() {
        let file = LocalCopyFileKind::File;
        assert!(format!("{file:?}").contains("File"));

        let dir = LocalCopyFileKind::Directory;
        assert!(format!("{dir:?}").contains("Directory"));

        let symlink = LocalCopyFileKind::Symlink;
        assert!(format!("{symlink:?}").contains("Symlink"));
    }

    #[test]
    fn file_kind_equality_same_variant() {
        assert_eq!(LocalCopyFileKind::File, LocalCopyFileKind::File);
        assert_eq!(LocalCopyFileKind::Directory, LocalCopyFileKind::Directory);
        assert_eq!(LocalCopyFileKind::Symlink, LocalCopyFileKind::Symlink);
        assert_eq!(LocalCopyFileKind::Fifo, LocalCopyFileKind::Fifo);
        assert_eq!(LocalCopyFileKind::CharDevice, LocalCopyFileKind::CharDevice);
        assert_eq!(
            LocalCopyFileKind::BlockDevice,
            LocalCopyFileKind::BlockDevice
        );
        assert_eq!(LocalCopyFileKind::Socket, LocalCopyFileKind::Socket);
        assert_eq!(LocalCopyFileKind::Other, LocalCopyFileKind::Other);
    }

    #[test]
    fn file_kind_inequality_different_variants() {
        assert_ne!(LocalCopyFileKind::File, LocalCopyFileKind::Directory);
        assert_ne!(LocalCopyFileKind::Directory, LocalCopyFileKind::Symlink);
        assert_ne!(LocalCopyFileKind::Symlink, LocalCopyFileKind::Fifo);
        assert_ne!(LocalCopyFileKind::Fifo, LocalCopyFileKind::CharDevice);
        assert_ne!(
            LocalCopyFileKind::CharDevice,
            LocalCopyFileKind::BlockDevice
        );
        assert_ne!(LocalCopyFileKind::BlockDevice, LocalCopyFileKind::Socket);
        assert_ne!(LocalCopyFileKind::Socket, LocalCopyFileKind::Other);
    }

    // Under --fake-super a device/FIFO/socket is stored as a regular
    // placeholder file whose `user.rsync.%stat` xattr encodes the real
    // st_mode. The reporting override must map those mode bits back to the
    // virtual kind (so the itemized type char matches upstream's
    // get_stat_xattr sender override, xattrs.c:1135) while leaving a %stat
    // that still describes a regular file untouched.
    #[cfg(all(unix, feature = "xattr"))]
    #[test]
    fn from_stat_mode_maps_type_bits_to_kind() {
        assert_eq!(
            LocalCopyFileKind::from_stat_mode(0o020644),
            Some(LocalCopyFileKind::CharDevice)
        );
        assert_eq!(
            LocalCopyFileKind::from_stat_mode(0o060660),
            Some(LocalCopyFileKind::BlockDevice)
        );
        assert_eq!(
            LocalCopyFileKind::from_stat_mode(0o010644),
            Some(LocalCopyFileKind::Fifo)
        );
        assert_eq!(
            LocalCopyFileKind::from_stat_mode(0o140755),
            Some(LocalCopyFileKind::Socket)
        );
        assert_eq!(
            LocalCopyFileKind::from_stat_mode(0o120777),
            Some(LocalCopyFileKind::Symlink)
        );
        // A regular file leaves the kind unchanged (returns None).
        assert_eq!(LocalCopyFileKind::from_stat_mode(0o100644), None);
    }

    #[cfg(unix)]
    #[test]
    fn virtualize_fake_super_is_noop_when_disabled() {
        use std::fs;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("placeholder");
        fs::write(&path, b"x").unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();
        let snapshot = LocalCopyMetadata::from_metadata(&meta, None);
        // fake_super = false must never consult the xattr or change the kind.
        let virtualized = snapshot.virtualize_fake_super(&path, false);
        assert_eq!(virtualized.kind(), LocalCopyFileKind::File);
    }

    // A regular placeholder carrying a device `%stat` xattr must report as a
    // device once fake-super virtualisation runs, matching the upstream
    // sender itemize (`cD` instead of `>f`). Skips gracefully on filesystems
    // that reject `user.*` xattrs so CI on such hosts does not flake.
    #[cfg(all(unix, feature = "xattr"))]
    #[test]
    fn virtualize_fake_super_promotes_placeholder_to_device() {
        use std::fs;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("char");
        fs::write(&path, b"").unwrap();
        let stat = ::metadata::FakeSuperStat {
            mode: 0o020644,
            uid: 0,
            gid: 0,
            rdev: Some((41, 67)),
        };
        if ::metadata::store_fake_super(&path, &stat).is_err() {
            eprintln!("skipping: user.* xattrs unsupported on test filesystem");
            return;
        }
        let meta = fs::symlink_metadata(&path).unwrap();
        let snapshot =
            LocalCopyMetadata::from_metadata(&meta, None).virtualize_fake_super(&path, true);
        assert_eq!(snapshot.kind(), LocalCopyFileKind::CharDevice);
        assert_eq!(snapshot.mode(), Some(0o020644));
    }

    // upstream: flist.c:1419-1428 - `--copy-devices` reports a device as a
    // regular file. The virtualisation must flip the kind to `File`, adopt the
    // supplied device size, and rewrite the mode's type bits to `S_IFREG` while
    // preserving the permission bits, so `--list-only` renders `-rw-...` with
    // the real byte length instead of `crw-.../brw-...` and `0`.
    #[cfg(unix)]
    #[test]
    fn virtualize_copy_device_as_file_reports_regular_file() {
        use std::fs;
        use std::os::unix::fs::FileTypeExt;

        let dev = Path::new("/dev/zero");
        let Ok(meta) = fs::symlink_metadata(dev) else {
            eprintln!("skipping: /dev/zero unavailable");
            return;
        };
        if !meta.file_type().is_char_device() {
            eprintln!("skipping: /dev/zero is not a char device here");
            return;
        }

        let snapshot = LocalCopyMetadata::from_metadata(&meta, None);
        assert_eq!(snapshot.kind(), LocalCopyFileKind::CharDevice);

        let virtualized = snapshot.virtualize_copy_device_as_file(4096);
        assert_eq!(virtualized.kind(), LocalCopyFileKind::File);
        assert_eq!(virtualized.len(), 4096);
        // Type bits are S_IFREG; permission bits are preserved from the device.
        assert_eq!(virtualized.mode().map(|m| m & 0o170_000), Some(0o100_000));
    }

    // The virtualisation is a strict no-op for a regular file: kind, length,
    // and mode are untouched so ordinary transfers are unaffected.
    #[test]
    fn virtualize_copy_device_as_file_noop_for_regular_file() {
        use std::fs;
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("regular");
        fs::write(&path, b"payload").unwrap();
        let meta = fs::symlink_metadata(&path).unwrap();

        let snapshot = LocalCopyMetadata::from_metadata(&meta, None);
        let len_before = snapshot.len();
        let virtualized = snapshot.virtualize_copy_device_as_file(999);
        assert_eq!(virtualized.kind(), LocalCopyFileKind::File);
        assert_eq!(virtualized.len(), len_before);
    }
}
