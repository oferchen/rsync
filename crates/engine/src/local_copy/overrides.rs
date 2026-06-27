use std::fs;
use std::io;
use std::path::Path;

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
type HardLinkOverrideFn = dyn Fn(&Path, &Path) -> io::Result<()> + 'static;

#[cfg(test)]
type DeviceIdOverrideFn = dyn Fn(&Path, &fs::Metadata) -> Option<u64> + 'static;

#[cfg(test)]
thread_local! {
    static HARD_LINK_OVERRIDE: RefCell<Option<Box<HardLinkOverrideFn>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_hard_link_override<F, R>(override_fn: F, action: impl FnOnce() -> R) -> R
where
    F: Fn(&Path, &Path) -> io::Result<()> + 'static,
{
    struct ResetGuard;

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            HARD_LINK_OVERRIDE.with(|cell| {
                cell.replace(None);
            });
        }
    }

    HARD_LINK_OVERRIDE.with(|cell| {
        cell.replace(Some(Box::new(override_fn)));
    });
    let guard = ResetGuard;
    let result = action();
    drop(guard);
    result
}

/// Creates a hard link from `source` to `destination`.
///
/// On Linux 5.15+ with io_uring available, the link is submitted as an
/// `IORING_OP_LINKAT` SQE instead of a synchronous `link(2)` syscall.
/// Falls back to `std::fs::hard_link` on all other platforms or when the
/// kernel lacks the opcode.
pub(super) fn create_hard_link(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = HARD_LINK_OVERRIDE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|override_fn| override_fn(source, destination))
    }) {
        return result;
    }

    fast_io::hard_link(source, destination)
}

#[cfg(test)]
thread_local! {
    static DEVICE_ID_OVERRIDE: RefCell<Option<Box<DeviceIdOverrideFn>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_device_id_override<F, R>(override_fn: F, action: impl FnOnce() -> R) -> R
where
    F: Fn(&Path, &fs::Metadata) -> Option<u64> + 'static,
{
    struct ResetGuard;

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            DEVICE_ID_OVERRIDE.with(|cell| {
                cell.replace(None);
            });
        }
    }

    DEVICE_ID_OVERRIDE.with(|cell| {
        cell.replace(Some(Box::new(override_fn)));
    });
    let guard = ResetGuard;
    let result = action();
    drop(guard);
    result
}

pub(super) fn device_identifier(path: &Path, metadata: &fs::Metadata) -> Option<u64> {
    #[cfg(test)]
    if let Some(value) = DEVICE_ID_OVERRIDE.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|override_fn| override_fn(path, metadata))
    }) {
        return Some(value);
    }

    #[cfg(not(test))]
    let _ = path;

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(metadata.dev())
    }

    #[cfg(windows)]
    {
        use std::borrow::Cow;
        use std::hash::{Hash, Hasher};
        use std::path::{Component, Prefix};

        fn normalize_path<'a>(path: &'a Path) -> Cow<'a, Path> {
            if path.is_absolute() {
                Cow::Borrowed(path)
            } else {
                std::env::current_dir()
                    .map(|cwd| Cow::Owned(cwd.join(path)))
                    .unwrap_or_else(|_| Cow::Borrowed(path))
            }
        }

        fn device_from_components(path: &Path) -> Option<u64> {
            let mut components = path.components();
            match components.next()? {
                Component::Prefix(prefix) => match prefix.kind() {
                    Prefix::Disk(letter) | Prefix::VerbatimDisk(letter) => Some(letter as u64),
                    Prefix::UNC(server, share) | Prefix::VerbatimUNC(server, share) => {
                        let mut hasher = std::collections::hash_map::DefaultHasher::new();
                        server.hash(&mut hasher);
                        share.hash(&mut hasher);
                        Some(hasher.finish())
                    }
                    _ => None,
                },
                _ => None,
            }
        }

        let absolute = normalize_path(path);
        // The standard library's `MetadataExt::volume_serial_number` accessor is currently
        // unstable on Windows, so fall back to deriving the device identifier from the path
        // components instead.
        let _ = metadata;

        device_from_components(absolute.as_ref())
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        let _ = metadata;
        None
    }
}

/// Returns whether `source` and `destination` reside on the same filesystem.
///
/// Compares the device id of `source` against the device id of
/// `destination`'s parent directory - the destination file itself may not
/// exist yet, so its filesystem is the parent's. Returns `None` when either
/// device id cannot be determined (e.g. the parent cannot be stat'd), letting
/// callers fall back to attempting the operation. Used to gate reflink fast
/// paths (`ioctl(FICLONE)`/`FICLONERANGE`) that only clone within a single
/// filesystem and otherwise fail with `EXDEV`.
#[cfg(target_os = "linux")]
pub(super) fn same_filesystem(
    source: &Path,
    source_metadata: &fs::Metadata,
    destination: &Path,
) -> Option<bool> {
    let source_dev = device_identifier(source, source_metadata)?;
    let parent = destination.parent()?;
    let parent_metadata = fs::symlink_metadata(parent).ok()?;
    let dest_dev = device_identifier(parent, &parent_metadata)?;
    Some(source_dev == dest_dev)
}

#[cfg(all(test, target_os = "linux"))]
mod same_filesystem_tests {
    use super::{same_filesystem, with_device_id_override};
    use tempfile::tempdir;

    #[test]
    fn reports_same_device_when_ids_match() {
        // Source and destination parent both resolve to device 1: the helper
        // must report Some(true) so the FICLONE fast path proceeds.
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        let dest = temp.path().join("dst");
        std::fs::write(&source, b"data").expect("write source");
        let source_meta = std::fs::symlink_metadata(&source).expect("source metadata");

        let verdict = with_device_id_override(
            |_path, _meta| Some(1),
            || same_filesystem(&source, &source_meta, &dest),
        );
        assert_eq!(verdict, Some(true));
    }

    #[test]
    fn reports_cross_device_when_ids_differ() {
        // Source resolves to device 1, the destination's parent to device 2:
        // the helper must report Some(false) so the caller skips the doomed
        // cross-filesystem FICLONE ioctl entirely.
        let source_dir = tempdir().expect("source tempdir");
        let dest_dir = tempdir().expect("dest tempdir");
        let source = source_dir.path().join("src");
        let dest = dest_dir.path().join("dst");
        std::fs::write(&source, b"data").expect("write source");
        let source_meta = std::fs::symlink_metadata(&source).expect("source metadata");

        let dest_parent = dest_dir.path().to_path_buf();
        let verdict = with_device_id_override(
            move |path, _meta| {
                if path == dest_parent.as_path() {
                    Some(2)
                } else {
                    Some(1)
                }
            },
            || same_filesystem(&source, &source_meta, &dest),
        );
        assert_eq!(verdict, Some(false));
    }

    #[test]
    fn reports_none_when_destination_parent_missing() {
        // A destination whose parent directory does not exist cannot be
        // stat'd, so the helper returns None and the caller falls through to
        // try_ficlone rather than wrongly skipping it.
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        std::fs::write(&source, b"data").expect("write source");
        let source_meta = std::fs::symlink_metadata(&source).expect("source metadata");
        let dest = temp.path().join("missing").join("dst");

        let verdict = with_device_id_override(
            |_path, _meta| Some(1),
            || same_filesystem(&source, &source_meta, &dest),
        );
        assert_eq!(verdict, None);
    }
}
