use std::fs;
use std::io;
use std::path::Path;

#[cfg(target_os = "linux")]
use std::collections::HashMap;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(test)]
use std::cell::RefCell;

#[cfg(test)]
type HardLinkOverrideFn = dyn Fn(&Path, &Path) -> io::Result<()> + 'static;

#[cfg(test)]
type DeviceIdOverrideFn = dyn Fn(&Path, &fs::Metadata) -> Option<u64> + 'static;

#[cfg(test)]
type BackupRenameOverrideFn = dyn Fn(&Path, &Path) -> Option<io::Result<()>> + 'static;

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
/// Delegates to `fast_io::hard_link`, which issues a direct `linkat(2)` /
/// `link(2)` syscall on every platform, matching upstream's
/// `hlink.c:hard_link_one()` -> `syscall.c:do_link_at()`.
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

/// Hard-links a destination entry into the backup area.
///
/// Same test-override seam as [`create_hard_link`], but on Unix production
/// resolves both endpoints through the ownership walk, bound to the session's
/// confinement root ([`fast_io::operator_link_confined`]). Ownership alone is
/// not enough: a non-chrooted daemon owns everything it creates, so a directory
/// symlink standing at the `--backup-dir` is trusted-owned by construction and
/// is followed by design; only the root can refuse where it lands.
/// A backup area is an operator path, and the link
/// tier runs *before* the rename tier, so confining only [`backup_rename`]
/// would leave upstream's `backup-dir-symlink-race` escape wide open: the
/// attacker's flipped parent redirects the link and the rename is never
/// reached.
///
/// This is deliberately NOT folded into [`create_hard_link`]: that one also
/// serves `-H` hardlink materialisation, which is a *transfer* path and takes
/// the confined resolver, not the ownership one.
///
/// upstream: `rsync-3.5.0/backup.c:200-207` `link_or_rename()`;
/// `syscall.c:676` `do_link_at()` under `operator_path_resolve`.
pub(super) fn create_backup_hard_link(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = HARD_LINK_OVERRIDE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|override_fn| override_fn(source, destination))
    }) {
        return result;
    }

    #[cfg(unix)]
    {
        fast_io::operator_link_confined(source, destination)
    }
    #[cfg(not(unix))]
    {
        fast_io::hard_link(source, destination)
    }
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

#[cfg(test)]
thread_local! {
    static BACKUP_RENAME_OVERRIDE: RefCell<Option<Box<BackupRenameOverrideFn>>> =
        const { RefCell::new(None) };
}

#[cfg(test)]
// Only the `#[cfg(unix)]` cross-device backup tests install this override; on
// non-Unix targets it is unreferenced.
#[cfg_attr(not(unix), allow(dead_code))]
pub(crate) fn with_backup_rename_override<F, R>(override_fn: F, action: impl FnOnce() -> R) -> R
where
    F: Fn(&Path, &Path) -> Option<io::Result<()>> + 'static,
{
    struct ResetGuard;

    impl Drop for ResetGuard {
        fn drop(&mut self) {
            BACKUP_RENAME_OVERRIDE.with(|cell| {
                cell.replace(None);
            });
        }
    }

    BACKUP_RENAME_OVERRIDE.with(|cell| {
        cell.replace(Some(Box::new(override_fn)));
    });
    let guard = ResetGuard;
    let result = action();
    drop(guard);
    result
}

/// Renames a destination entry to its backup location.
///
/// On Unix both endpoints are resolved by the ownership walk
/// ([`fast_io::operator_rename_confined`]): every path component is inspected
/// without following it, a symlink owned by uid 0 or our euid is followed as the
/// operator's own layout, and one owned by anyone else is refused. A backup
/// directory may legitimately sit outside the transfer tree, so location cannot
/// be the trust signal for WHETHER TO FOLLOW - authority is. It is still the
/// signal for whether the landing site is acceptable when the session has a
/// confinement root, which is what stops a followed trusted symlink from
/// carrying an in-module file out of a served module.
///
/// Without that, an attacker who can create entries inside the backup tree
/// flips a parent component between a real directory and a symlink pointing
/// outside, and the path-based rename lands the backup wherever the symlink
/// pointed at the instant the kernel resolved it - upstream's
/// `backup-dir-symlink-race`.
///
/// upstream: `rsync-3.5.0/backup.c:200-219` `make_backup()` sets
/// `operator_path_resolve` around the rename; `syscall.c:1894` `do_rename_at()`
/// then walks each side with `owner_walk_parent()`.
///
/// In tests a thread-local override can force a specific outcome (e.g. an
/// `EXDEV` cross-device error) to exercise the copy-tree fallback without a
/// real second filesystem.
pub(super) fn backup_rename(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = BACKUP_RENAME_OVERRIDE.with(|cell| {
        cell.borrow()
            .as_ref()
            .and_then(|override_fn| override_fn(from, to))
    }) {
        return result;
    }

    #[cfg(unix)]
    {
        fast_io::operator_rename_confined(from, to, true)
    }
    #[cfg(not(unix))]
    {
        fs::rename(from, to)
    }
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
        // Native Windows has no POSIX st_dev. Use the volume serial number
        // (GetFileInformationByHandle) as the stable per-volume identity so a
        // junction or a volume mounted at a directory is detected as a
        // filesystem boundary - a drive-letter path prefix cannot see it.
        // upstream: flist.c `st.st_dev != filesystem_dev` mount-point check.
        let _ = metadata;
        fast_io::same_fs::volume_id(path)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        let _ = metadata;
        None
    }
}

/// Returns whether `source` shares a filesystem with a destination whose
/// parent directory has device id `dest_parent_device`.
///
/// The destination file itself may not exist yet, so its filesystem is the
/// parent's; callers resolve the parent device - ideally from a cache to avoid
/// a redundant per-file `statx` - and pass it here. Returns `None` when either
/// device id is unavailable, letting callers attempt the operation and fall
/// back on `EXDEV`. Used to gate reflink fast paths (`ioctl(FICLONE)`/
/// `FICLONERANGE`) that only clone within a single filesystem.
#[cfg(target_os = "linux")]
pub(super) fn same_filesystem(
    source: &Path,
    source_metadata: &fs::Metadata,
    dest_parent_device: Option<u64>,
) -> Option<bool> {
    let source_dev = device_identifier(source, source_metadata)?;
    let dest_dev = dest_parent_device?;
    Some(source_dev == dest_dev)
}

/// Resolves the device id of `parent`, memoizing it in the verified-parent
/// cache so sibling files in one directory pay a single `statx`.
///
/// `parent` is normally already present in `cache` (inserted by
/// `prepare_parent_directory` with a `None` device placeholder). On a cache
/// hit with a resolved device the stored id is returned with no syscall; on a
/// placeholder the directory is stat'd once and the id written back. When the
/// parent is absent from the cache - only reachable outside the normal
/// per-file flow - the directory is stat'd without caching, matching the prior
/// uncached behaviour. Returns `None` when the directory cannot be stat'd or
/// its device id is unavailable.
#[cfg(target_os = "linux")]
pub(super) fn cached_parent_device(
    cache: &mut HashMap<PathBuf, Option<u64>>,
    parent: &Path,
) -> Option<u64> {
    if let Some(Some(dev)) = cache.get(parent) {
        return Some(*dev);
    }
    let metadata = fs::symlink_metadata(parent).ok()?;
    let dev = device_identifier(parent, &metadata)?;
    // Only fill an existing verified entry, so an unverified parent is never
    // recorded as verified. A FICLONE-path parent has already been through
    // prepare_parent_directory, so the entry exists in the common case.
    if let Some(slot) = cache.get_mut(parent) {
        *slot = Some(dev);
    }
    Some(dev)
}

#[cfg(all(test, target_os = "linux"))]
mod same_filesystem_tests {
    use super::{
        cached_parent_device, device_identifier, same_filesystem, with_device_id_override,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tempfile::tempdir;

    #[test]
    fn reports_same_device_when_ids_match() {
        // Source device 1 and a resolved parent device 1: the helper must
        // report Some(true) so the FICLONE fast path proceeds.
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        std::fs::write(&source, b"data").expect("write source");
        let source_meta = std::fs::symlink_metadata(&source).expect("source metadata");

        let verdict = with_device_id_override(
            |_path, _meta| Some(1),
            || same_filesystem(&source, &source_meta, Some(1)),
        );
        assert_eq!(verdict, Some(true));
    }

    #[test]
    fn reports_cross_device_when_ids_differ() {
        // Source device 1, parent device 2: the helper must report Some(false)
        // so the caller skips the doomed cross-filesystem FICLONE ioctl.
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        std::fs::write(&source, b"data").expect("write source");
        let source_meta = std::fs::symlink_metadata(&source).expect("source metadata");

        let verdict = with_device_id_override(
            |_path, _meta| Some(1),
            || same_filesystem(&source, &source_meta, Some(2)),
        );
        assert_eq!(verdict, Some(false));
    }

    #[test]
    fn reports_none_when_parent_device_unknown() {
        // An unresolved parent device makes the filesystem indeterminate, so
        // the helper returns None and the caller falls through to try_ficlone
        // rather than wrongly skipping it.
        let temp = tempdir().expect("tempdir");
        let source = temp.path().join("src");
        std::fs::write(&source, b"data").expect("write source");
        let source_meta = std::fs::symlink_metadata(&source).expect("source metadata");

        let verdict = with_device_id_override(
            |_path, _meta| Some(1),
            || same_filesystem(&source, &source_meta, None),
        );
        assert_eq!(verdict, None);
    }

    #[test]
    fn cached_parent_device_returns_stored_id_without_statting() {
        // A verified parent whose device is already resolved must be served
        // from the cache alone: the path does not exist on disk, so any statx
        // would fail and yield None. A hit proves no syscall occurred - the
        // per-file redundant parent statx is eliminated.
        let mut cache: HashMap<PathBuf, Option<u64>> = HashMap::new();
        let parent = PathBuf::from("/nonexistent/verified/parent");
        cache.insert(parent.clone(), Some(4242));

        assert_eq!(cached_parent_device(&mut cache, &parent), Some(4242));
    }

    #[test]
    fn cached_parent_device_resolves_once_then_memoizes() {
        // First call on a placeholder entry stats the directory and writes the
        // device back; a second call is served from the cache even after the
        // directory is removed, proving the stat happens once per directory,
        // not once per file.
        let temp = tempdir().expect("tempdir");
        let parent = temp.path().to_path_buf();
        let mut cache: HashMap<PathBuf, Option<u64>> = HashMap::new();
        cache.insert(parent.clone(), None);

        let expected = device_identifier(
            &parent,
            &std::fs::symlink_metadata(&parent).expect("parent metadata"),
        )
        .expect("device id");
        let first = cached_parent_device(&mut cache, &parent).expect("device resolved");
        assert_eq!(first, expected);
        assert_eq!(cache.get(&parent), Some(&Some(expected)));

        // Remove the directory: a re-stat would now fail, so a cache miss would
        // return None. The cached value must still be returned.
        drop(temp);
        assert_eq!(cached_parent_device(&mut cache, &parent), Some(expected));
    }

    #[test]
    fn cached_parent_device_does_not_cache_unverified_parent() {
        // A parent absent from the cache (not verified this transfer) is stat'd
        // but not inserted, so it never masquerades as a verified directory.
        let temp = tempdir().expect("tempdir");
        let parent = temp.path().to_path_buf();
        let mut cache: HashMap<PathBuf, Option<u64>> = HashMap::new();

        assert!(cached_parent_device(&mut cache, &parent).is_some());
        assert!(cache.is_empty());
    }
}
