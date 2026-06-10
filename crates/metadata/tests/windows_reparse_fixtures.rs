//! Windows reparse-point fixture helpers and classifier integration tests.
//!
//! Feeds the WPC-8 reparse classifier
//! (`metadata::windows::reparse::classify_reparse_point`) with realistic
//! `mklink`- and `mountvol`-created reparse points. Provides three RAII-style
//! fixtures so individual test cases can request a specific NTFS reparse
//! shape and have it cleaned up automatically on drop:
//!
//! - [`DirSymlinkFixture`]    -> `mklink /d <link> <target>`
//!                               (`IO_REPARSE_TAG_SYMLINK`)
//! - [`JunctionFixture`]      -> `mklink /j <link> <target>`
//!                               (`IO_REPARSE_TAG_MOUNT_POINT`, non-volume
//!                               substitute-name)
//! - [`MountPointFixture`]    -> `mountvol <link> <volume_guid>`
//!                               (`IO_REPARSE_TAG_MOUNT_POINT`, volume
//!                               substitute-name)
//!
//! `mklink /d` and `mountvol` require either administrator privileges or
//! Windows 10 developer mode; the fixtures surface the privilege failure as
//! an `io::Error` and individual tests downgrade to a runtime skip rather
//! than failing the suite. `mklink /j` works without elevation on Windows
//! 10+ and runs unconditionally, mirroring the in-tree integration test
//! shipped with the classifier itself.
//!
//! # Standard-library-first policy
//!
//! All three fixtures shell out to OS-provided utilities (`cmd.exe` for
//! `mklink`, `mountvol.exe` for the volume mount-point) via
//! `std::process::Command`. The metadata crate already pulls in the
//! `windows` crate for the classifier's FFI surface, but the test fixtures
//! deliberately avoid touching FFI so they can act as a pure-`std`
//! reference for callers who want to create reparse points outside the
//! metadata crate.

#![cfg(target_os = "windows")]

use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use metadata::windows::reparse::{ReparseKind, classify_reparse_point};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};

/// `FILE_READ_ATTRIBUTES` access mask (`winnt.h`). Sufficient for
/// `FSCTL_GET_REPARSE_POINT` and avoids the broader rights `GENERIC_READ`
/// would request, keeping the open path side-effect free.
const FILE_READ_ATTRIBUTES: u32 = 0x0080;

/// RAII fixture creating a directory symbolic link via
/// `cmd.exe /c mklink /d <link> <target>`.
///
/// `mklink /d` produces a reparse point with `IO_REPARSE_TAG_SYMLINK`
/// (`0xA000000C`) so the classifier returns [`ReparseKind::Symlink`].
/// Requires the `SeCreateSymbolicLinkPrivilege` privilege, which on
/// Windows 10+ ships with administrator accounts or with developer mode
/// enabled (Settings -> Update & Security -> For developers). When the
/// privilege is missing `mklink` exits non-zero; the constructor maps the
/// failure to [`io::ErrorKind::PermissionDenied`] and callers should
/// downgrade to a runtime skip rather than panicking.
///
/// On drop, the symlink is removed with [`std::fs::remove_dir`], which is
/// the correct primitive for both directory junctions and `mklink /d`
/// symlinks on Windows (the reparse point is the directory itself, not a
/// file).
pub struct DirSymlinkFixture {
    link: PathBuf,
}

impl DirSymlinkFixture {
    /// Create a directory symlink at `link` that points at `target`.
    ///
    /// Returns [`io::ErrorKind::PermissionDenied`] when `mklink /d`
    /// refuses the operation (most often because the caller lacks
    /// `SeCreateSymbolicLinkPrivilege`), so tests can skip cleanly
    /// without failing the suite.
    pub fn new(link: &Path, target: &Path) -> io::Result<Self> {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/d"])
            .arg(link)
            .arg(target)
            .status()?;
        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mklink /d requires administrator or developer mode",
            ));
        }
        Ok(Self {
            link: link.to_path_buf(),
        })
    }

    /// Path to the created symlink.
    pub fn path(&self) -> &Path {
        &self.link
    }
}

impl Drop for DirSymlinkFixture {
    fn drop(&mut self) {
        // Windows treats directory symlinks as directories at the
        // filesystem layer; `remove_dir` deletes the reparse point
        // itself without touching the target.
        let _ = std::fs::remove_dir(&self.link);
    }
}

/// RAII fixture creating a directory junction via
/// `cmd.exe /c mklink /j <link> <target>`.
///
/// Junctions use `IO_REPARSE_TAG_MOUNT_POINT` (`0xA0000003`) with a
/// non-volume substitute-name (typically `\??\C:\path\to\target`), so
/// the classifier returns [`ReparseKind::Junction`]. Junction creation
/// does not require elevation on Windows 10+, so the constructor only
/// returns an error when `cmd.exe` is unavailable or the underlying
/// `mklink` invocation fails for filesystem reasons.
pub struct JunctionFixture {
    link: PathBuf,
}

impl JunctionFixture {
    /// Create a junction at `link` that points at `target`.
    ///
    /// Returns [`io::ErrorKind::Other`] when `mklink /j` exits non-zero
    /// (rare on Windows 10+ since junctions do not require privilege
    /// elevation).
    pub fn new(link: &Path, target: &Path) -> io::Result<Self> {
        let status = Command::new("cmd")
            .args(["/c", "mklink", "/j"])
            .arg(link)
            .arg(target)
            .status()?;
        if !status.success() {
            return Err(io::Error::other(format!(
                "mklink /j {} {} exited with {status:?}",
                link.display(),
                target.display()
            )));
        }
        Ok(Self {
            link: link.to_path_buf(),
        })
    }

    /// Path to the created junction.
    pub fn path(&self) -> &Path {
        &self.link
    }
}

impl Drop for JunctionFixture {
    fn drop(&mut self) {
        // Junctions are directory reparse points; `remove_dir` removes
        // the reparse without recursing into the target.
        let _ = std::fs::remove_dir(&self.link);
    }
}

/// RAII fixture creating a volume mount-point via
/// `mountvol.exe <mount_point_dir> <volume_guid>`.
///
/// Mount-points share `IO_REPARSE_TAG_MOUNT_POINT` with junctions but
/// use a `\??\Volume{GUID}\` substitute-name, so the classifier returns
/// [`ReparseKind::MountPoint`]. `mountvol` requires administrator
/// privileges; without elevation the constructor returns
/// [`io::ErrorKind::PermissionDenied`] and tests skip rather than fail.
///
/// On drop the mount-point is dismounted with `mountvol /d`, which
/// detaches the volume without affecting its data.
pub struct MountPointFixture {
    link: PathBuf,
}

impl MountPointFixture {
    /// Mount the volume identified by `volume_guid_path` (for example
    /// `\\?\Volume{12345678-1234-1234-1234-123456789abc}\`) at
    /// `mount_point_dir`. The mount-point directory must already exist
    /// and be empty, matching `mountvol`'s preconditions.
    ///
    /// Returns [`io::ErrorKind::PermissionDenied`] when `mountvol`
    /// exits non-zero, which most often indicates the caller is not
    /// running as administrator. Callers should downgrade to a runtime
    /// skip rather than panicking.
    pub fn new(mount_point_dir: &Path, volume_guid_path: &str) -> io::Result<Self> {
        let status = Command::new("mountvol")
            .arg(mount_point_dir)
            .arg(volume_guid_path)
            .status()?;
        if !status.success() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "mountvol requires administrator privileges",
            ));
        }
        Ok(Self {
            link: mount_point_dir.to_path_buf(),
        })
    }

    /// Path to the created mount-point directory.
    pub fn path(&self) -> &Path {
        &self.link
    }
}

impl Drop for MountPointFixture {
    fn drop(&mut self) {
        // `mountvol /d` detaches the volume from the directory without
        // touching the volume contents. Ignore failures: the test may
        // already have removed the directory, or `mountvol` may be
        // unavailable in the cleanup environment.
        let _ = Command::new("mountvol")
            .arg(&self.link)
            .arg("/d")
            .status();
        let _ = std::fs::remove_dir(&self.link);
    }
}

/// Owns a Win32 file handle obtained from `CreateFileW` and closes it on
/// drop. Mirrors the helper used inside the classifier's in-tree
/// integration test so this file can call [`classify_reparse_point`]
/// against the same handle shape the production caller would supply.
struct OwnedHandle(HANDLE);

impl std::os::windows::io::AsRawHandle for OwnedHandle {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.0 as std::os::windows::io::RawHandle
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: `self.0` was returned by `CreateFileW` in `open_reparse`
        // and is owned uniquely by this guard. The handle has not been
        // closed elsewhere.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.0);
        }
    }
}

/// Open `path` with `FILE_FLAG_OPEN_REPARSE_POINT |
/// FILE_FLAG_BACKUP_SEMANTICS` so the reparse data is returned by
/// `FSCTL_GET_REPARSE_POINT` instead of being followed. Backup-semantics
/// is required to open directory reparse points (junctions, mount-points,
/// `mklink /d` symlinks).
fn open_reparse(path: &Path) -> io::Result<OwnedHandle> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide` is a null-terminated UTF-16 path slice owned for the
    // duration of the call. `FILE_FLAG_OPEN_REPARSE_POINT` ensures the
    // reparse data is returned instead of followed; `FILE_FLAG_BACKUP_SEMANTICS`
    // is required to open directories. The returned `HANDLE` is wrapped
    // in [`OwnedHandle`] for guaranteed close-on-drop.
    let handle = unsafe {
        CreateFileW(
            windows::core::PCWSTR(wide.as_ptr()),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
            None,
        )
    };

    let handle = handle.map_err(|_| io::Error::last_os_error())?;
    Ok(OwnedHandle(handle))
}

/// Query `mountvol` for a volume GUID path that can be remounted at a
/// fresh directory. Returns the first `\\?\Volume{...}\` line emitted by
/// `mountvol` with no arguments; on failure (`mountvol` unavailable, no
/// volumes listed, output unparseable) returns `None` so the calling
/// test can downgrade to a runtime skip.
fn first_volume_guid_path() -> Option<String> {
    let output = Command::new("mountvol").output().ok()?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\\\\?\\Volume{") && trimmed.ends_with('\\') {
            return Some(trimmed.to_string());
        }
    }
    None
}

#[test]
fn classify_returns_symlink_for_dir_symlink() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("real_dir");
    std::fs::create_dir(&target).expect("create target dir");
    let link = dir.path().join("a_symlink");

    let fixture = match DirSymlinkFixture::new(&link, &target) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("skipping: mklink /d unavailable ({err})");
            return;
        }
    };

    let handle = open_reparse(fixture.path()).expect("open dir symlink reparse");
    let kind = classify_reparse_point(&handle).expect("classify dir symlink");
    assert_eq!(
        kind,
        ReparseKind::Symlink,
        "mklink /d should produce IO_REPARSE_TAG_SYMLINK"
    );
}

#[test]
fn classify_returns_junction_for_mklink_j() {
    let dir = tempfile::tempdir().expect("tempdir");
    let target = dir.path().join("real_dir");
    std::fs::create_dir(&target).expect("create target dir");
    let link = dir.path().join("a_junction");

    let fixture = match JunctionFixture::new(&link, &target) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("skipping: mklink /j unavailable ({err})");
            return;
        }
    };

    let handle = open_reparse(fixture.path()).expect("open junction reparse");
    let kind = classify_reparse_point(&handle).expect("classify junction");
    assert_eq!(
        kind,
        ReparseKind::Junction,
        "mklink /j should produce IO_REPARSE_TAG_MOUNT_POINT with a directory substitute-name"
    );
}

#[test]
fn classify_returns_mount_point_for_setvolumemountpoint() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mount_dir = dir.path().join("a_mount");
    std::fs::create_dir(&mount_dir).expect("create mount target dir");

    let Some(volume_guid) = first_volume_guid_path() else {
        eprintln!("skipping: no volume GUID available from mountvol");
        return;
    };

    let fixture = match MountPointFixture::new(&mount_dir, &volume_guid) {
        Ok(f) => f,
        Err(err) => {
            eprintln!("skipping: mountvol unavailable or non-admin ({err})");
            return;
        }
    };

    let handle = open_reparse(fixture.path()).expect("open mount-point reparse");
    let kind = classify_reparse_point(&handle).expect("classify mount-point");
    assert_eq!(
        kind,
        ReparseKind::MountPoint,
        "mountvol mount should produce IO_REPARSE_TAG_MOUNT_POINT with a \\??\\Volume{{...}}\\ substitute-name"
    );
}
