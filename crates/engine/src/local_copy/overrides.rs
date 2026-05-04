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

pub(super) fn create_hard_link(source: &Path, destination: &Path) -> io::Result<()> {
    #[cfg(test)]
    if let Some(result) = HARD_LINK_OVERRIDE.with(|cell| {
        cell.borrow()
            .as_ref()
            .map(|override_fn| override_fn(source, destination))
    }) {
        return result;
    }

    fs::hard_link(source, destination)
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
