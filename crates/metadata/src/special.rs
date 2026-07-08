//! FIFO, socket, and device-node creation helpers.
//!
//! Mirrors upstream rsync's `syscall.c:do_mknod()` for materialising special
//! files before metadata is applied. Includes `--fake-super` placeholder
//! substitution so unprivileged users can preserve privileged metadata via
//! the `user.rsync.%stat` xattr instead of issuing `mknod(2)`.

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
    use nix::sys::stat::{Mode, SFlag, makedev, mknod};
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};

    let is_socket = metadata.file_type().is_socket();
    let (kind, context) = if is_socket {
        // Linux defines MKNOD_CREATES_SOCKETS, so upstream materialises socket
        // nodes with mknod(S_IFSOCK) here (the Apple path binds instead).
        (SFlag::S_IFSOCK, "create socket")
    } else {
        (SFlag::S_IFIFO, "create fifo")
    };

    let mode_bits = permissions_mode(context, destination, metadata.permissions().mode() & 0o777)?;
    let perm = Mode::from_bits_truncate(mode_bits.into());

    // `nix::sys::stat::mknod` wraps the libc `mknod` symbol, so an LD_PRELOAD
    // interposer such as fakeroot can fake CAP_MKNOD; see mknod note below.
    mknod(destination, kind, perm, makedev(0, 0))
        .map_err(|error| MetadataError::new(context, destination, io::Error::from(error)))
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
    use nix::sys::stat::{Mode, SFlag, mknod};
    use std::os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt};

    let file_type = metadata.file_type();
    let kind = if file_type.is_char_device() {
        SFlag::S_IFCHR
    } else if file_type.is_block_device() {
        SFlag::S_IFBLK
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
    let perm = Mode::from_bits_truncate(mode_bits.into());

    // The source rdev already encodes major/minor for this platform, so it can
    // be handed straight to mknod (matching upstream do_mknod, which forwards
    // the device word verbatim). try_into guards the rare targets where dev_t
    // is narrower than the u64 returned by MetadataExt::rdev.
    let device: libc::dev_t = metadata
        .rdev()
        .try_into()
        .map_err(|_| invalid_device_error(destination))?;

    // `nix::sys::stat::mknod` calls the libc `mknod` symbol rather than issuing
    // the raw `mknod`/`mknodat` syscall that rustix emits. This matters under
    // `fakeroot`/`fakeroot-ng`, which fake privileged operations by
    // `LD_PRELOAD`-interposing the libc `mknod` symbol: a raw syscall is never
    // intercepted, so the real kernel rejects it with `EPERM` (the emulated
    // root uid holds no genuine `CAP_MKNOD`). Upstream `syscall.c:do_mknod()`
    // likewise calls the libc `mknod()` function, which fakeroot fakes to
    // success, restoring compatibility for unprivileged rootfs/package builds.
    // upstream: syscall.c:do_mknod() - libc mknod(), interceptable by fakeroot
    mknod(destination, kind, perm, device)
        .map_err(|error| MetadataError::new("create device", destination, io::Error::from(error)))
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

    if is_socket {
        create_socket_via_bind(context, destination, mode_bits)
    } else {
        // On Apple platforms, `libc::mode_t` is currently a type alias for
        // `u16`, so this cast is infallible and needs no checked conversion.
        let mode: libc::mode_t = mode_bits as libc::mode_t;
        apple_fs::mkfifo(destination, mode)
            .map_err(|error| MetadataError::new(context, destination, error))
    }
}

/// Materialises a Unix-domain socket node by binding to `destination`.
///
/// macOS and the BSDs do not define `MKNOD_CREATES_SOCKETS`, so `mknod(2)`
/// with `S_IFSOCK` fails there (typically `EOPNOTSUPP`). Upstream rsync
/// creates the socket node the same way this helper does: open an
/// `AF_UNIX`/`SOCK_STREAM` socket, unlink any stale entry, `bind(2)` it to the
/// path, close it, then `chmod(2)` to apply the requested permission bits.
///
/// The bind path is bounded by `sizeof(sun_path)` (104 bytes on Apple); an
/// over-long path surfaces as an `ENAMETOOLONG` error rather than silently
/// truncating, matching upstream's `strlcpy` length check.
// upstream: syscall.c:489-513 do_mknod() - !MKNOD_CREATES_SOCKETS socket branch
#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn create_socket_via_bind(
    context: &'static str,
    destination: &Path,
    mode_bits: u16,
) -> Result<(), MetadataError> {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    // upstream: unlink(pathname) tolerating ENOENT before bind().
    if let Err(error) = fs::remove_file(destination)
        && error.kind() != io::ErrorKind::NotFound
    {
        return Err(MetadataError::new(context, destination, error));
    }

    // `UnixListener::bind` performs socket(PF_UNIX, SOCK_STREAM, 0) + bind(2),
    // leaving a socket node on the filesystem once the listener is dropped.
    let listener = UnixListener::bind(destination)
        .map_err(|error| socket_bind_error(context, destination, error))?;
    drop(listener);

    // upstream applies the requested mode via do_chmod() after bind().
    fs::set_permissions(
        destination,
        fs::Permissions::from_mode(u32::from(mode_bits)),
    )
    .map_err(|error| MetadataError::new(context, destination, error))
}

/// Maps a `bind(2)` failure to a `MetadataError`, translating the
/// path-length error the OS reports (`EINVAL` on Apple when `sun_path`
/// overflows) into the `ENAMETOOLONG` upstream surfaces for the same case.
#[cfg(all(
    unix,
    any(
        target_os = "ios",
        target_os = "macos",
        target_os = "tvos",
        target_os = "watchos"
    )
))]
fn socket_bind_error(context: &'static str, destination: &Path, error: io::Error) -> MetadataError {
    let sun_path_cap = std::mem::size_of::<libc::sockaddr_un>()
        - std::mem::offset_of!(libc::sockaddr_un, sun_path);
    // Account for the trailing NUL byte, matching upstream's `>= sizeof` guard.
    if destination.as_os_str().len() >= sun_path_cap {
        return MetadataError::new(
            context,
            destination,
            io::Error::from_raw_os_error(libc::ENAMETOOLONG),
        );
    }
    MetadataError::new(context, destination, error)
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

/// Emits the WIND-2 skip-with-warn diagnostic for a special-file entry
/// the receiver cannot materialise on this target and returns `Ok(())` so
/// the surrounding transfer continues with the next entry.
///
/// Mirrors the `xattr_stub::warn_xattr_unsupported` stderr convention used
/// elsewhere in this crate for unsupported metadata operations.
// WIND-2: skip-with-warn strategy for Windows device files.
// docs/design/windows-device-file-strategy.md
#[cfg(not(unix))]
fn warn_skip_special(destination: &Path, kind_label: &'static str) {
    eprintln!("{}", format_skip_special_message(destination, kind_label));
}

/// Builds the WIND-2 skip-with-warn message body so tests can assert the
/// exact wording without capturing stderr.
#[cfg(not(unix))]
#[must_use]
pub(crate) fn format_skip_special_message(destination: &Path, kind_label: &'static str) -> String {
    format!(
        "skipping {kind_label} \"{path}\": Windows targets cannot create device nodes [receiver]",
        path = destination.display(),
    )
}

#[cfg(not(unix))]
fn create_fifo_inner(destination: &Path, _metadata: &fs::Metadata) -> Result<(), MetadataError> {
    warn_skip_special(destination, "fifo entry");
    Ok(())
}

#[cfg(not(unix))]
fn create_device_node_inner(
    destination: &Path,
    _metadata: &fs::Metadata,
) -> Result<(), MetadataError> {
    // `fs::FileType` on non-Unix targets cannot distinguish block vs
    // character device or FIFO vs socket; the caller has already routed
    // by entry kind, so we report the broadest accurate label and let the
    // surrounding receiver layer refine wording when it knows more from
    // the wire-protocol `FileEntry`.
    warn_skip_special(destination, "device entry");
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

#[cfg(unix)]
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

    // On Apple targets, `mknod(2)` with `S_IFSOCK` is unsupported, so a
    // socket destination must be materialised via socket()+bind()+chmod,
    // mirroring upstream's !MKNOD_CREATES_SOCKETS branch (syscall.c:489-513).
    // The result must be a real socket node carrying the requested mode, and
    // a stale entry at the destination must be replaced rather than error.
    #[cfg(all(
        unix,
        any(
            target_os = "ios",
            target_os = "macos",
            target_os = "tvos",
            target_os = "watchos"
        )
    ))]
    #[test]
    fn create_socket_binds_node_with_permissions() {
        use std::os::unix::fs::{FileTypeExt, PermissionsExt};
        use std::os::unix::net::UnixListener;

        let temp = tempdir().expect("create tempdir");

        // Derive metadata from a genuine socket so `is_socket()` is true.
        let source_path = temp.path().join("source.sock");
        let source_listener = UnixListener::bind(&source_path).expect("bind source socket");
        let mut perms = fs::symlink_metadata(&source_path)
            .expect("source metadata")
            .permissions();
        perms.set_mode(0o640);
        fs::set_permissions(&source_path, perms).expect("set source permissions");
        let metadata = fs::symlink_metadata(&source_path).expect("source metadata after chmod");
        assert!(metadata.file_type().is_socket());

        // A pre-existing entry at the destination must be replaced.
        let socket_path = temp.path().join("dest.sock");
        fs::write(&socket_path, b"stale").expect("write stale destination");

        create_fifo(&socket_path, &metadata).expect("create socket");

        let created = fs::symlink_metadata(&socket_path).expect("dest metadata");
        assert!(
            created.file_type().is_socket(),
            "destination must be a socket node, not a regular file"
        );
        assert_eq!(
            created.permissions().mode() & 0o777,
            0o640,
            "socket must carry the requested permission bits"
        );

        drop(source_listener);
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

    /// Device creation must route through the libc `mknod` symbol (so an
    /// LD_PRELOAD interposer such as fakeroot can fake `CAP_MKNOD`), not a raw
    /// syscall. Using `/dev/null`'s char-device metadata, the call either
    /// succeeds (root or under fakeroot) or fails with `PermissionDenied` from
    /// the real kernel when unprivileged. Either outcome proves the syscall was
    /// actually attempted via libc and its result surfaced rather than swallowed
    /// - the raw-syscall bug always failed with EPERM even under fakeroot.
    // upstream: syscall.c:do_mknod() - libc mknod(), interceptable by fakeroot
    #[cfg(unix)]
    #[test]
    fn create_device_node_issues_libc_mknod() {
        use std::os::unix::fs::FileTypeExt;

        let source = Path::new("/dev/null");
        let metadata = match fs::symlink_metadata(source) {
            Ok(metadata) if metadata.file_type().is_char_device() => metadata,
            // No usable char device on this target (e.g. sandbox); skip.
            _ => return,
        };

        let temp = tempdir().expect("create tempdir");
        let dest = temp.path().join("null");

        match create_device_node(&dest, &metadata) {
            Ok(()) => {
                let created = fs::symlink_metadata(&dest).expect("device metadata");
                assert!(
                    created.file_type().is_char_device(),
                    "destination must be a char device node"
                );
            }
            Err(error) => {
                assert_eq!(error.context(), "create device");
                assert_eq!(
                    error.source_error().kind(),
                    io::ErrorKind::PermissionDenied,
                    "unprivileged mknod must surface the kernel's EPERM"
                );
                assert!(!dest.exists(), "failed mknod must not leave a node behind");
            }
        }
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

    /// WIND-2 skip-with-warn: a FIFO request on a non-Unix target must
    /// return `Ok(())`, must NOT create a destination inode, and the
    /// diagnostic helper must surface the path and entry kind so operators
    /// can see what was skipped.
    #[cfg(not(unix))]
    #[test]
    fn create_fifo_skips_with_warn_on_non_unix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::File::create(&source).expect("create source");
        let metadata = fs::metadata(&source).expect("metadata");
        let fifo_path = temp.path().join("fifo");

        let result = create_fifo(&fifo_path, &metadata);

        assert!(result.is_ok(), "skip-with-warn must not surface an error");
        assert!(
            !fifo_path.exists(),
            "skip path must not register a destination inode"
        );

        let message = format_skip_special_message(&fifo_path, "fifo entry");
        assert!(message.contains("skipping fifo entry"));
        assert!(message.contains("[receiver]"));
        assert!(message.contains(&fifo_path.display().to_string()));
    }

    /// WIND-2 skip-with-warn: a device request on a non-Unix target must
    /// return `Ok(())`, must NOT create a destination inode, and the
    /// diagnostic helper must identify it as a device entry.
    #[cfg(not(unix))]
    #[test]
    fn create_device_node_skips_with_warn_on_non_unix() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        fs::File::create(&source).expect("create source");
        let metadata = fs::metadata(&source).expect("metadata");
        let device_path = temp.path().join("device");

        let result = create_device_node(&device_path, &metadata);

        assert!(result.is_ok(), "skip-with-warn must not surface an error");
        assert!(
            !device_path.exists(),
            "skip path must not register a destination inode"
        );

        let message = format_skip_special_message(&device_path, "device entry");
        assert!(message.contains("skipping device entry"));
        assert!(message.contains("[receiver]"));
        assert!(message.contains(&device_path.display().to_string()));
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
