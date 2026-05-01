use crate::error::MetadataError;
use std::fs;
use std::io;
use std::path::Path;

/// Creates a special file node (FIFO or socket) at `destination` matching
/// the source metadata, so that further metadata can be applied afterwards.
///
/// In upstream rsync, `--specials` covers both named pipes (FIFOs) and
/// Unix-domain sockets.  When the source is a socket the destination is
/// recreated as a socket node; when the source is a FIFO the destination
/// is recreated as a FIFO.
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

/// Creates a FIFO or socket at `destination`, honouring `--fake-super`.
///
/// When `fake_super` is `true`, mirrors upstream `syscall.c:do_mknod()`'s
/// `am_root < 0` branch: instead of issuing `mknod(2)`, a regular `0600`
/// placeholder file is created so an unprivileged process can preserve the
/// node's metadata in `user.rsync.%stat` (written separately by
/// `store_fake_super`). When `fake_super` is `false`, behaviour matches
/// [`create_fifo`].
///
/// # Errors
///
/// Returns [`MetadataError`] if the placeholder cannot be created or if the
/// underlying mknod call fails.
// upstream: syscall.c:do_mknod() - placeholder substitution when am_root < 0
pub fn create_fifo_with_fake_super(
    destination: &Path,
    metadata: &fs::Metadata,
    fake_super: bool,
) -> Result<(), MetadataError> {
    if fake_super {
        create_fake_super_placeholder(destination, "create fifo")
    } else {
        create_fifo_inner(destination, metadata)
    }
}

/// Creates a device node at `destination`, honouring `--fake-super`.
///
/// When `fake_super` is `true`, mirrors upstream `syscall.c:do_mknod()`'s
/// `am_root < 0` branch: instead of issuing `mknod(2)` (which requires
/// `CAP_MKNOD`), a regular `0600` placeholder file is created so an
/// unprivileged process can preserve the device's privileged metadata
/// (mode, uid, gid, rdev) in `user.rsync.%stat` (written separately by
/// `store_fake_super`). When `fake_super` is `false`, behaviour matches
/// [`create_device_node`].
///
/// # Errors
///
/// Returns [`MetadataError`] if the placeholder cannot be created or if the
/// underlying mknod call fails.
// upstream: syscall.c:do_mknod() - placeholder substitution when am_root < 0
pub fn create_device_node_with_fake_super(
    destination: &Path,
    metadata: &fs::Metadata,
    fake_super: bool,
) -> Result<(), MetadataError> {
    if fake_super {
        create_fake_super_placeholder(destination, "create device")
    } else {
        create_device_node_inner(destination, metadata)
    }
}

/// Creates an empty 0600 regular file used as a fake-super placeholder.
///
/// Upstream `do_mknod()` performs the equivalent substitution by routing the
/// call through `do_open` with `O_CREAT|O_WRONLY|O_EXCL` and mode `0600`. Any
/// pre-existing entry at `destination` is removed first to mirror the
/// `unlink + create` semantics used by upstream when overwriting an existing
/// special-file destination.
// upstream: syscall.c:90-174 - do_mknod() routes to do_open when am_root < 0
fn create_fake_super_placeholder(
    destination: &Path,
    context: &'static str,
) -> Result<(), MetadataError> {
    if let Err(error) = fs::remove_file(destination)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(MetadataError::new(context, destination, error));
    }

    let mut open_options = fs::OpenOptions::new();
    open_options.write(true).create_new(true);
    apply_placeholder_mode(&mut open_options);
    open_options
        .open(destination)
        .map(drop)
        .map_err(|error| MetadataError::new(context, destination, error))
}

#[cfg(unix)]
fn apply_placeholder_mode(open_options: &mut fs::OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;
    open_options.mode(0o600);
}

#[cfg(not(unix))]
fn apply_placeholder_mode(_open_options: &mut fs::OpenOptions) {
    // On non-Unix targets there is no POSIX mode to apply; the placeholder is
    // a regular file with platform-default permissions.
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
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let node_type = if metadata.file_type().is_socket() {
        FileType::Socket
    } else {
        FileType::Fifo
    };

    let context = if matches!(node_type, FileType::Socket) {
        "create socket"
    } else {
        "create fifo"
    };

    let mode_bits = permissions_mode(context, destination, metadata.permissions().mode() & 0o777)?;
    let mode = Mode::from_bits_truncate(mode_bits.into());

    mknodat(CWD, destination, node_type, mode, makedev(0, 0))
        .map_err(|error| MetadataError::new(context, destination, io::Error::from(error)))?;

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
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let is_socket = metadata.file_type().is_socket();
    let context = if is_socket {
        "create socket"
    } else {
        "create fifo"
    };

    let mode_bits = permissions_mode(context, destination, metadata.permissions().mode() & 0o777)?;
    // On Apple platforms, `libc::mode_t` is currently a type alias for `u16`,
    // so this cast is infallible and does not require a checked conversion.
    let mode: libc::mode_t = mode_bits as libc::mode_t;

    if is_socket {
        // On Apple platforms, create a socket node via mknod with S_IFSOCK.
        let full_mode = libc::S_IFSOCK | mode;
        apple_fs::mknod(destination, full_mode, 0)
            .map_err(|error| MetadataError::new(context, destination, error))
    } else {
        apple_fs::mkfifo(destination, mode)
            .map_err(|error| MetadataError::new(context, destination, error))
    }
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
    // As above, `libc::mode_t` is a `u16` alias on Apple targets,
    // so this is an infallible cast and cannot overflow.
    let permissions: libc::mode_t = perm_bits as libc::mode_t;
    let device: libc::dev_t = metadata
        .rdev()
        .try_into()
        .map_err(|_| invalid_device_error(destination))?;
    let mode = type_bits | permissions;

    apple_fs::mknod(destination, mode, device)
        .map_err(|error| MetadataError::new("create device", destination, error))
}

#[cfg(not(unix))]
fn create_fifo_inner(destination: &Path, _metadata: &fs::Metadata) -> Result<(), MetadataError> {
    let _ = destination;
    Ok(())
}

#[cfg(not(unix))]
fn create_device_node_inner(
    destination: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    let _ = destination;
    Ok(())
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
    #[cfg(unix)]
    use super::*;
    #[cfg(unix)]
    use std::fs;
    #[cfg(unix)]
    use std::io;
    #[cfg(unix)]
    use std::path::Path;
    #[cfg(unix)]
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

    #[cfg(not(unix))]
    #[test]
    fn create_fifo_noop_on_non_unix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::File::create(&source).expect("create source");
        let metadata = fs::metadata(&source).expect("metadata");
        let fifo_path = temp.path().join("fifo");
        let result = create_fifo(&fifo_path, &metadata);
        assert!(result.is_ok());
    }

    #[cfg(not(unix))]
    #[test]
    fn create_device_node_noop_on_non_unix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::File::create(&source).expect("create source");
        let metadata = fs::metadata(&source).expect("metadata");
        let device_path = temp.path().join("device");
        let result = create_device_node(&device_path, &metadata);
        assert!(result.is_ok());
    }

    /// Under `--fake-super`, `create_fifo_with_fake_super` must never call
    /// `mknod(2)`. Instead it creates a regular 0600 placeholder so the
    /// destination's would-be metadata can be captured separately in the
    /// `user.rsync.%stat` xattr.
    /// // upstream: syscall.c:do_mknod() under am_root < 0
    #[cfg(unix)]
    #[test]
    fn fake_super_replaces_mkfifo_with_regular_placeholder() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let temp = tempdir().expect("create tempdir");
        let source_path = temp.path().join("source");
        fs::File::create(&source_path).expect("create source");
        let metadata = fs::metadata(&source_path).expect("metadata");

        let dest = temp.path().join("placeholder.fifo");
        create_fifo_with_fake_super(&dest, &metadata, true).expect("placeholder created");

        let dest_meta = fs::symlink_metadata(&dest).expect("placeholder metadata");
        assert!(
            dest_meta.file_type().is_file(),
            "fake-super must create a regular file, not a fifo"
        );
        assert!(!dest_meta.file_type().is_fifo());
        assert_eq!(dest_meta.permissions().mode() & 0o777, 0o600);
    }

    /// Same invariant for `create_device_node_with_fake_super`: never
    /// invoke `mknod(2)` for a device, fall back to a 0600 placeholder.
    /// // upstream: syscall.c:do_mknod() under am_root < 0
    #[cfg(unix)]
    #[test]
    fn fake_super_replaces_mknod_with_regular_placeholder() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let temp = tempdir().expect("create tempdir");
        let source_path = temp.path().join("source");
        fs::File::create(&source_path).expect("create source");
        let metadata = fs::metadata(&source_path).expect("metadata");

        let dest = temp.path().join("placeholder.dev");
        create_device_node_with_fake_super(&dest, &metadata, true)
            .expect("placeholder created without CAP_MKNOD");

        let dest_meta = fs::symlink_metadata(&dest).expect("placeholder metadata");
        assert!(
            dest_meta.file_type().is_file(),
            "fake-super must create a regular file, not a device node"
        );
        assert!(!dest_meta.file_type().is_block_device());
        assert!(!dest_meta.file_type().is_char_device());
        assert_eq!(dest_meta.permissions().mode() & 0o777, 0o600);
    }

    /// When fake-super is disabled, `create_fifo_with_fake_super` falls
    /// through to the real mknod path (subject to the same platform
    /// availability as `create_fifo`).
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
    fn create_fifo_with_fake_super_disabled_creates_real_fifo() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};

        let temp = tempdir().expect("create tempdir");
        let source_path = temp.path().join("source");
        fs::File::create(&source_path).expect("create source");
        let mut permissions = fs::metadata(&source_path).expect("metadata").permissions();
        permissions.set_mode(0o640);
        fs::set_permissions(&source_path, permissions).expect("set permissions");
        let metadata = fs::metadata(&source_path).expect("metadata after permissions");

        let dest = temp.path().join("real.fifo");
        create_fifo_with_fake_super(&dest, &metadata, false).expect("real fifo created");

        let dest_meta = fs::symlink_metadata(&dest).expect("fifo metadata");
        assert!(dest_meta.file_type().is_fifo());
    }
}
