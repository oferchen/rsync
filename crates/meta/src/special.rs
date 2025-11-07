use crate::error::MetadataError;
use std::fs;
use std::io;
use std::path::Path;

/// Creates a FIFO at `destination` so metadata can be applied afterwards.
pub fn create_fifo(destination: &Path, metadata: &fs::Metadata) -> Result<(), MetadataError> {
    create_fifo_inner(destination, metadata)
}

/// Creates a device node at `destination` that mirrors the supplied metadata.
pub fn create_device_node(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    create_device_node_inner(destination, metadata)
}

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
fn create_fifo_inner(destination: &Path, metadata: &fs::Metadata) -> Result<(), MetadataError> {
    use rustix::fs::{CWD, makedev, mknodat};
    use rustix::fs::{FileType, Mode};
    use std::os::unix::fs::PermissionsExt;

    let mode_bits = permissions_mode(
        "create fifo",
        destination,
        metadata.permissions().mode() & 0o777,
    )?;
    let mode = Mode::from_bits_truncate(mode_bits.into());

    mknodat(CWD, destination, FileType::Fifo, mode, makedev(0, 0))
        .map_err(|error| MetadataError::new("create fifo", destination, io::Error::from(error)))?;

    Ok(())
}

#[cfg(all(
    unix,
    not(any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    ))
))]
fn create_device_node_inner(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    use rustix::fs::{CWD, major, minor};
    use rustix::fs::{Dev, FileType, Mode, makedev, mknodat};
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

    let mode_bits = permissions_mode(
        "create device",
        destination,
        metadata.permissions().mode() & 0o777,
    )?;
    let mode = Mode::from_bits_truncate(mode_bits.into());
    let raw: Dev = metadata.rdev();
    let device = makedev(major(raw), minor(raw));

    mknodat(CWD, destination, node_type, mode, device).map_err(|error| {
        MetadataError::new("create device", destination, io::Error::from(error))
    })?;

    Ok(())
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn create_fifo_inner(destination: &Path, metadata: &fs::Metadata) -> Result<(), MetadataError> {
    use std::convert::TryInto;
    use std::os::unix::fs::PermissionsExt;

    let mode_bits = permissions_mode(
        "create fifo",
        destination,
        metadata.permissions().mode() & 0o777,
    )?;
    let mode: libc::mode_t = mode_bits
        .try_into()
        .map_err(|_| invalid_mode_error("create fifo", destination))?;

    oc_rsync_apple_fs::mkfifo(destination, mode)
        .map_err(|error| MetadataError::new("create fifo", destination, error))
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn create_device_node_inner(
    destination: &Path,
    metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    use std::convert::TryInto;
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let file_type = metadata.file_type();
    let type_bits: libc::mode_t = if file_type.is_char_device() {
        libc::S_IFCHR
    } else if file_type.is_block_device() {
        libc::S_IFBLK
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

    let perm_bits = permissions_mode(
        "create device",
        destination,
        metadata.permissions().mode() & 0o777,
    )?;
    let permissions: libc::mode_t = perm_bits
        .try_into()
        .map_err(|_| invalid_mode_error("create device", destination))?;
    let device: libc::dev_t = metadata
        .rdev()
        .try_into()
        .map_err(|_| invalid_device_error(destination))?;
    let mode = type_bits | permissions;

    oc_rsync_apple_fs::mknod(destination, mode, device)
        .map_err(|error| MetadataError::new("create device", destination, error))
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

#[cfg(unix)]
fn permissions_mode(
    context: &'static str,
    destination: &Path,
    raw_mode: u32,
) -> Result<u16, MetadataError> {
    use std::convert::TryFrom;

    let masked = raw_mode & 0o177_777;
    u16::try_from(masked).map_err(|_| invalid_mode_error(context, destination))
}

#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn invalid_device_error(destination: &Path) -> MetadataError {
    MetadataError::new(
        "create device",
        destination,
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "device identifier exceeds platform limits",
        ),
    )
}

#[cfg(unix)]
fn invalid_mode_error(context: &'static str, destination: &Path) -> MetadataError {
    MetadataError::new(
        context,
        destination,
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "mode value exceeds platform limits",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io;
    use std::path::Path;
    use tempfile::tempdir;

    #[cfg(all(
        unix,
        not(any(
            target_os = "ios",
            target_os = "macos",
            target_os = "tvos",
            target_os = "watchos"
        ))
    ))]
    #[test]
    fn create_fifo_applies_metadata_permissions() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let temp = tempdir().expect("create tempdir");
        let source_path = temp.path().join("source");
        fs::File::create(&source_path).expect("create metadata source");

        let mut permissions = fs::metadata(&source_path).expect("metadata").permissions();
        permissions.set_mode(0o640);
        fs::set_permissions(&source_path, permissions).expect("set permissions");
        let metadata = fs::metadata(&source_path).expect("metadata after permissions");

        let fifo_path = temp.path().join("fifo");
        create_fifo(&fifo_path, &metadata).expect("create fifo");

        let fifo_metadata = fs::symlink_metadata(&fifo_path).expect("fifo metadata");
        assert!(fifo_metadata.file_type().is_fifo());

        let requested = metadata.permissions().mode() & 0o777;
        let created = fifo_metadata.permissions().mode() & 0o777;
        assert_ne!(created, 0, "fifo permissions should preserve some bits");
        assert_eq!(
            created & requested,
            created,
            "created permissions must not exceed requested"
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_device_node_rejects_non_device_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempdir().expect("create tempdir");
        let source_path = temp.path().join("regular");
        fs::File::create(&source_path).expect("create regular file");

        let mut permissions = fs::metadata(&source_path).expect("metadata").permissions();
        permissions.set_mode(0o600);
        fs::set_permissions(&source_path, permissions).expect("set permissions");
        let metadata = fs::metadata(&source_path).expect("metadata after permissions");

        let device_path = temp.path().join("device");
        let error = create_device_node(&device_path, &metadata)
            .expect_err("non-device metadata should fail");

        assert_eq!(error.context(), "create device");
        assert_eq!(error.path(), device_path.as_path());
        assert_eq!(error.source_error().kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(unix)]
    #[test]
    fn permissions_mode_masks_high_bits() {
        let mode = permissions_mode("test", Path::new("/tmp/target"), 0o777_777)
            .expect("mode conversion succeeds");
        assert_eq!(mode, 0o177_777);
    }

    #[cfg(unix)]
    #[test]
    fn invalid_mode_error_carries_context_and_path() {
        let path = Path::new("/tmp/example");
        let error = invalid_mode_error("example", path);

        assert_eq!(error.context(), "example");
        assert_eq!(error.path(), path);
        assert_eq!(error.source_error().kind(), io::ErrorKind::InvalidInput);
        assert!(
            error
                .source_error()
                .to_string()
                .contains("mode value exceeds platform limits")
        );
    }
}
