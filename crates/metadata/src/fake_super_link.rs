//! Fake-super symlink placeholders.
//!
//! Linux cannot set a user xattr on a symlink, so upstream builds there with
//! `NO_SYMLINK_USER_XATTRS` (configure.ac:1466). A `--fake-super` receiver
//! then stores each link as a regular placeholder file. The file's content
//! is the target and its `%stat` records the link type and owner.

use std::io::{self, Read, Write};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

const S_IFMT: u32 = 0o170000;
const S_IFREG: u32 = 0o100000;
const S_IFLNK: u32 = 0o120000;
/// upstream: generator.c:659 - quick_check_ok() reads at most MAXPATHLEN-1.
const MAX_TARGET_LEN: u64 = 4095;

/// Reports whether a `--fake-super` receiver stores symlinks as placeholders.
///
/// True on Linux only, where upstream defines `NO_SYMLINK_USER_XATTRS`.
pub const fn symlinks_are_placeholders(fake_super: bool) -> bool {
    cfg!(target_os = "linux") && fake_super
}

/// Reads the target of the placeholder link at `link_path`.
///
/// A placeholder is a regular file whose `%stat` records `S_IFLNK`. A real
/// symlink there reads as an empty target, so the caller replaces it with a
/// placeholder. The leaf is opened with `O_NOFOLLOW`, anchored at `sandbox`
/// when one holds `dest_dir`, else through the confined fallback.
///
/// # Errors
///
/// Fails when nothing at `link_path` is a placeholder link; the caller then
/// treats the path as an obstacle. Returns the stat, open or read error.
// upstream: generator.c:1965 quick_check_ok(FT_SYMLINK) on the x_lstat()
// type, and syscall.c:do_readlink_atfd() - a real symlink reads as length 0
// on a fake-super receiver.
pub fn read_fake_super_link(
    sandbox: Option<&fast_io::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
) -> io::Result<PathBuf> {
    let on_disk =
        fast_io::lstat_via_sandbox_or_fallback(sandbox, dest_dir, relative_path, link_path)?;
    if on_disk.mode() & S_IFMT == S_IFLNK {
        return Ok(PathBuf::new());
    }
    let is_link_placeholder = on_disk.mode() & S_IFMT == S_IFREG
        && crate::fake_super::load_fake_super(link_path)
            .ok()
            .flatten()
            .is_some_and(|stat| stat.mode & S_IFMT == S_IFLNK);
    if !is_link_placeholder {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a fake-super symlink placeholder",
        ));
    }
    let mut target = Vec::new();
    fast_io::openat_via_sandbox_or_fallback(
        sandbox,
        dest_dir,
        relative_path,
        link_path,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        0,
    )?
    .take(MAX_TARGET_LEN)
    .read_to_end(&mut target)?;
    Ok(PathBuf::from(std::ffi::OsString::from_vec(target)))
}

/// Writes a placeholder link to `target` at `link_path`.
///
/// The leaf is opened with `O_NOFOLLOW`, so a planted symlink cannot redirect
/// the write, and is anchored like [`read_fake_super_link`].
///
/// # Errors
///
/// Returns the open or write error.
// upstream: syscall.c:do_symlink_atfd() - `am_root < 0` creates the
// placeholder with openat(O_WRONLY|O_CREAT|O_TRUNC|O_NOFOLLOW, 0600) and
// writes the target into it.
pub fn create_fake_super_link(
    sandbox: Option<&fast_io::DirSandbox>,
    dest_dir: &Path,
    relative_path: &Path,
    link_path: &Path,
    target: &Path,
) -> io::Result<()> {
    fast_io::openat_via_sandbox_or_fallback(
        sandbox,
        dest_dir,
        relative_path,
        link_path,
        libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        0o600,
    )?
    .write_all(target.as_os_str().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[cfg(feature = "xattr")]
    fn xattrs_supported(dir: &Path) -> bool {
        let probe = dir.join(".probe");
        std::fs::write(&probe, b"").expect("write probe");
        let ok = xattr::set(&probe, "user.probe", b"1").is_ok();
        std::fs::remove_file(&probe).expect("remove probe");
        ok
    }

    #[cfg(feature = "xattr")]
    fn mark_as_link(path: &Path) {
        let stat = crate::fake_super::FakeSuperStat {
            mode: 0o120777,
            uid: 0,
            gid: 0,
            rdev: None,
        };
        crate::fake_super::store_fake_super(path, &stat).expect("store %stat");
    }

    #[test]
    fn only_a_linux_fake_super_receiver_uses_placeholders() {
        assert_eq!(symlinks_are_placeholders(true), cfg!(target_os = "linux"));
        assert!(!symlinks_are_placeholders(false));
    }

    #[test]
    fn placeholder_create_writes_the_target_into_a_0600_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let link = dir.path().join("link");
        create_fake_super_link(
            None,
            dir.path(),
            Path::new("link"),
            &link,
            Path::new("/x/y"),
        )
        .expect("create placeholder");
        let meta = std::fs::symlink_metadata(&link).expect("stat placeholder");
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o7777, 0o600);
        assert_eq!(std::fs::read(&link).expect("read"), b"/x/y");
    }

    #[test]
    fn placeholder_create_refuses_to_write_through_a_planted_symlink() {
        // upstream: syscall.c:do_symlink_atfd() - O_NOFOLLOW on the leaf.
        let dir = tempfile::tempdir().expect("tempdir");
        let victim = dir.path().join("victim");
        std::fs::write(&victim, b"keep").expect("write victim");
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&victim, &link).expect("plant symlink");
        let err =
            create_fake_super_link(None, dir.path(), Path::new("link"), &link, Path::new("t"))
                .expect_err("a planted symlink must not be followed");
        assert_eq!(err.raw_os_error(), Some(libc::ELOOP));
        assert_eq!(std::fs::read(&victim).expect("read victim"), b"keep");
    }

    #[cfg(feature = "xattr")]
    #[test]
    fn placeholder_read_returns_the_content_of_a_link_placeholder() {
        let dir = tempfile::tempdir().expect("tempdir");
        if !xattrs_supported(dir.path()) {
            return;
        }
        let link = dir.path().join("link");
        std::fs::write(&link, b"f").expect("write placeholder");
        mark_as_link(&link);
        let target = read_fake_super_link(None, dir.path(), Path::new("link"), &link)
            .expect("read placeholder");
        assert_eq!(target, PathBuf::from("f"));
    }

    #[test]
    fn placeholder_read_rejects_a_plain_file_and_empties_a_real_symlink() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("plain");
        std::fs::write(&plain, b"f").expect("write plain file");
        assert!(
            read_fake_super_link(None, dir.path(), Path::new("plain"), &plain).is_err(),
            "a file without a S_IFLNK %stat is an obstacle, not a link"
        );
        let real = dir.path().join("real");
        std::os::unix::fs::symlink("f", &real).expect("create symlink");
        assert_eq!(
            read_fake_super_link(None, dir.path(), Path::new("real"), &real)
                .expect("read real symlink"),
            PathBuf::new(),
            "a real symlink reads as length 0 so it is replaced"
        );
    }
}
