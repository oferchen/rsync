//! Ownership-trusted resolution for **operator-supplied** paths.
//!
//! An operator path - a `--backup-dir`, `--temp-dir`, `--partial-dir` or
//! alt-dest the person running rsync named - may legitimately point outside the
//! transfer tree, so the confined walk in [`crate::dir_sandbox`] is the wrong
//! policy for it: location cannot be the trust signal. Upstream's answer is to
//! make **authority** the signal instead - follow a symlink owned by uid 0 or
//! our own euid (the operator's own layout), refuse any other-uid one, at every
//! component.
//!
//! That distinction is the whole defence in upstream's
//! `backup-dir-symlink-race` test: an attacker who can create entries inside the
//! backup tree flips a parent component between a real directory and a symlink
//! pointing outside, and a path-based rename lands the backup wherever the
//! symlink pointed at the instant the kernel resolved it.
//!
//! # Upstream Reference
//!
//! - `rsync-3.5.0/syscall.c:558` `owner_walk_parent()` - open the parent of an
//!   operator path via the ownership walk, hand back the final component.
//! - `rsync-3.5.0/syscall.c:286` `ona_open()` - the per-component walk itself.
//! - `rsync-3.5.0/syscall.c:406` - the ownership test:
//!   `if (lst.st_uid != 0 && lst.st_uid != trusted_uid)` refuse with `ELOOP`.
//! - `rsync-3.5.0/syscall.c:544-551` - "An operator path may legitimately point
//!   outside the tree, so the trust signal is authority (ownership), not
//!   location."

use std::ffi::{OsStr, OsString};
use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

use rustix::fs::{AtFlags, FileType, Mode, OFlags};

/// Symlink-follow budget for one walk, spent across every component.
///
/// upstream: `rsync-3.5.0/syscall.c:361` `int loops = 40;` - "SYMLOOP_MAX-ish;
/// breaks symlink cycles. Counts symlink expansions only, NOT path depth."
const MAX_SYMLINK_HOPS: u32 = 40;

/// Effective uid, the second trusted owner alongside root.
///
/// upstream: `rsync-3.5.0/syscall.c:304` `const uid_t trusted_uid = geteuid();`
fn trusted_uid() -> u32 {
    // SAFETY: `geteuid(2)` takes no arguments, cannot fail, and returns a plain
    // integer. It is one of the few POSIX calls with no error path at all.
    #[allow(unsafe_code)]
    unsafe {
        libc::geteuid()
    }
}

/// Is a symlink owned by `uid` the operator's own, and therefore followable?
///
/// Ownership - not location - is the trust signal for an operator-supplied
/// path: uid 0 or our own euid is the operator's own layout (the
/// `/backup -> /mnt/disk` admin pattern), any other uid is an attacker's
/// plant. An operator path may legitimately point outside the tree, which is
/// why the confined beneath-walk is the wrong resolver for it.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:406` - `st_uid != 0 && st_uid != trusted_uid`
///   refuses the symlink; otherwise the walk follows it.
/// - `rsync-3.5.0/util1.c:1216` `change_dir()` - resolves the operator-named
///   destination with `open_no_attacker_symlinks()` on exactly this rule.
#[must_use]
pub fn symlink_owner_is_trusted(uid: u32) -> bool {
    uid == 0 || uid == trusted_uid()
}

/// Open a directory component beneath `dirfd`, refusing a symlink at the leaf.
///
/// The caller has already `statat`'d the component and found it is not a
/// symlink; `O_NOFOLLOW` closes the window between that check and this open, so
/// a component flipped to a symlink in between fails rather than resolves.
fn open_dir_component(dirfd: BorrowedFd<'_>, name: &OsStr) -> io::Result<OwnedFd> {
    rustix::fs::openat(
        dirfd,
        name,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
}

/// Split `path` into the components of its parent plus the final name.
///
/// Returns `None` when the path has no final component (`/`, `.`, `..`, or
/// empty), which no operator path being renamed onto can have.
fn split_parent(path: &Path) -> Option<(Vec<OsString>, OsString)> {
    let leaf = path.file_name()?.to_os_string();
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let mut names = Vec::new();
    for component in parent.components() {
        match component {
            Component::Normal(name) => names.push(name.to_os_string()),
            Component::ParentDir => names.push(OsString::from("..")),
            // `/` is expressed by the walk starting at the root; `.` is a no-op;
            // a Windows prefix cannot occur on a Unix-only module.
            Component::RootDir | Component::CurDir | Component::Prefix(_) => {}
        }
    }
    Some((names, leaf))
}

/// Push the components of `path` onto the front of `pending`, in order.
fn prepend_components(pending: &mut Vec<OsString>, path: &Path) {
    let mut head: Vec<OsString> = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            _ => None,
        })
        .collect();
    head.append(pending);
    *pending = head;
}

/// Open the parent directory of `path` via the ownership walk.
///
/// Every component is inspected with `AT_SYMLINK_NOFOLLOW` before it is opened.
/// A symlink owned by uid 0 or the euid is followed (its target is spliced into
/// the remaining path, an absolute target restarting the walk at `/`); a symlink
/// owned by anyone else is refused.
///
/// Returns the parent descriptor plus the final component, ready for a `*at`
/// operation.
///
/// # Errors
///
/// - `ELOOP` when a component is a symlink owned by an untrusted uid, or the
///   hop budget is exhausted. This is the security refusal, and it is
///   deliberately not `EXDEV`: callers treat `EXDEV` as cross-device and fall
///   back to copy+remove, which would defeat the refusal.
/// - `EINVAL` when `path` has no final component.
/// - Otherwise the `openat`/`statat`/`readlinkat` errno verbatim.
pub fn owner_trusted_parent(path: &Path) -> io::Result<(OwnedFd, OsString)> {
    let Some((mut pending, leaf)) = split_parent(path) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };

    let start = if path.is_absolute() { "/" } else { "." };
    let mut dirfd = rustix::fs::open(
        start,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;

    let mut hops = MAX_SYMLINK_HOPS;

    while !pending.is_empty() {
        let name = pending.remove(0);
        let stat = rustix::fs::statat(dirfd.as_fd(), name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;

        if FileType::from_raw_mode(stat.st_mode as _) != FileType::Symlink {
            dirfd = open_dir_component(dirfd.as_fd(), name.as_os_str())?;
            continue;
        }

        // upstream: syscall.c:406 - an other-uid symlink is the attacker's, and
        // is refused; uid 0 or our own euid is the operator's own layout.
        if !symlink_owner_is_trusted(stat.st_uid) {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        if hops == 0 {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        hops -= 1;

        let target = rustix::fs::readlinkat(dirfd.as_fd(), name.as_os_str(), Vec::new())
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;
        let target = PathBuf::from(OsStr::from_bytes(target.as_bytes()));
        if target.is_absolute() {
            // upstream: syscall.c:422 "Absolute target restarts the walk from /".
            dirfd = rustix::fs::open(
                "/",
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))?;
        }
        prepend_components(&mut pending, &target);
    }

    Ok((dirfd, leaf))
}

/// Rename `old_path` to `new_path` with both endpoints resolved by the
/// ownership walk.
///
/// This is the operator-path counterpart to
/// [`confined_rename`](crate::confined_rename): that one confines beneath a
/// transfer root, this one trusts by ownership so an operator directory outside
/// the tree still works. Each side is walked independently, mirroring upstream.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:1894` `do_rename_at()` under `operator_path_resolve`
///   - `owner_walk_parent` on each side, then `renameat`.
/// - `rsync-3.5.0/backup.c:200-219` `make_backup()` - the caller that sets the
///   operator-path mode around the backup rename.
///
/// # Errors
///
/// Propagates the walk's refusal (`ELOOP`) or the `renameat(2)` errno.
pub fn operator_rename(old_path: &Path, new_path: &Path, replace: bool) -> io::Result<()> {
    let (old_dirfd, old_leaf) = owner_trusted_parent(old_path)?;
    let (new_dirfd, new_leaf) = owner_trusted_parent(new_path)?;
    crate::renameat(
        old_dirfd.as_fd(),
        &old_leaf,
        new_dirfd.as_fd(),
        &new_leaf,
        replace,
    )
}

/// Hard-link `old_path` to `new_path` with both endpoints resolved by the
/// ownership walk.
///
/// The link tier runs *before* the rename tier in a backup, so confining only
/// the rename would leave the escape wide open - upstream sets the
/// operator-path mode around `link_or_rename()` as a whole, covering both.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/backup.c:200-207` `link_or_rename()` - `do_link_at` first,
///   `do_rename_at` on failure.
/// - `rsync-3.5.0/syscall.c:676` `do_link_at()` under `operator_path_resolve` -
///   `owner_walk_parent` on each side, then `linkat`.
///
/// # Errors
///
/// Propagates the walk's refusal (`ELOOP`) or the `linkat(2)` errno - notably
/// `EXDEV` when the backup area is on another filesystem, which the caller
/// treats as its signal to fall through to the next tier.
pub fn operator_link(old_path: &Path, new_path: &Path) -> io::Result<()> {
    let (old_dirfd, old_leaf) = owner_trusted_parent(old_path)?;
    let (new_dirfd, new_leaf) = owner_trusted_parent(new_path)?;
    crate::linkat(old_dirfd.as_fd(), &old_leaf, new_dirfd.as_fd(), &new_leaf)
}

/// Open `path` with the parent resolved by the ownership walk and the leaf
/// refused if it is a symlink.
///
/// The two halves are deliberately different policies, and both are load-bearing:
///
/// - **parent components** go through [`owner_trusted_parent`], which follows a
///   symlink owned by uid 0 or our euid and refuses any other-uid one. This is
///   what defends the `/tmp/somedir/rsync.log` shape, where the leaf does not
///   exist yet so there is nothing for `O_NOFOLLOW` to reject.
/// - **the leaf** gets an unconditional `O_NOFOLLOW`, so a symlink *at* the named
///   path is refused whoever owns it.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:469` - the last component is opened
///   `openat(dfd, comp, flags | O_NOFOLLOW, mode)`: blanket `O_NOFOLLOW` on the
///   leaf, in contrast to the ownership test the walk applies to every parent.
/// - `rsync-3.5.0/syscall.c:537` `open_no_attacker_symlinks()` - the public
///   entry point this mirrors, used for the opens that are not confined beneath
///   a root (`--log-file`, `--*-from`, lock/motd - `syscall.c:232`).
///
/// # Errors
///
/// Propagates the walk's refusal (`ELOOP`) or the `openat(2)` errno - notably
/// `ELOOP` when the leaf itself is a symlink.
fn operator_open_with(path: &Path, flags: OFlags, mode: Mode) -> io::Result<std::fs::File> {
    let (dirfd, leaf) = owner_trusted_parent(path)?;
    rustix::fs::openat(
        dirfd.as_fd(),
        leaf.as_os_str(),
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )
    .map(std::fs::File::from)
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
}

/// Open an operator-supplied path read-only through the ownership walk.
///
/// The read-side counterpart for the operator paths rsync only consumes -
/// `--password-file`, `--exclude-from`/`--include-from`/`--files-from`, the
/// daemon config and secrets files.
///
/// # Errors
///
/// See [`operator_open_with`].
pub fn operator_open_read(path: &Path) -> io::Result<std::fs::File> {
    operator_open_with(path, OFlags::RDONLY, Mode::empty())
}

/// Open an operator-supplied path for appending, creating it if absent.
///
/// This is the `--log-file` shape, and the reason the leaf `O_NOFOLLOW` matters
/// as much as the walk: a privileged rsync appending through a planted symlink
/// writes attacker-chosen bytes into an attacker-chosen file.
///
/// # Errors
///
/// See [`operator_open_with`].
pub fn operator_open_append(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    operator_open_with(
        path,
        OFlags::WRONLY | OFlags::APPEND | OFlags::CREATE,
        // `RawMode` is u16 on macOS and u32 on Linux, so route the cast through
        // it rather than naming either width here.
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
    )
}

#[cfg(test)]
mod tests {
    use super::{operator_open_append, operator_open_read};
    use std::io::{Read, Write};
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    /// A plain path with no symlink anywhere resolves and opens normally - the
    /// walk must not break the ordinary case it guards.
    #[test]
    fn reads_a_plain_path_through_a_real_directory() {
        let temp = TempDir::new().expect("tempdir");
        let dir = temp.path().join("logs");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::write(dir.join("f"), b"payload").expect("write");

        let mut opened = operator_open_read(&dir.join("f")).expect("open");
        let mut body = String::new();
        opened.read_to_string(&mut body).expect("read");
        assert_eq!(body, "payload");
    }

    /// A symlink AT the named path is refused whoever owns it.
    ///
    /// upstream: `syscall.c:469` opens the last component with `flags |
    /// O_NOFOLLOW` - the leaf is not subject to the ownership test the walk
    /// applies to parents, it is refused outright. The plant here is owned by
    /// the running uid, which the *parent* rule would happily follow; that is
    /// what makes this cell discriminate leaf policy from parent policy.
    #[test]
    fn refuses_a_symlink_at_the_leaf_even_when_self_owned() {
        let temp = TempDir::new().expect("tempdir");
        let victim = temp.path().join("victim");
        std::fs::write(&victim, b"SENTINEL").expect("write victim");
        let plant = temp.path().join("log");
        symlink(&victim, &plant).expect("symlink");

        let refused = operator_open_read(&plant).expect_err("leaf symlink must be refused");
        assert_eq!(
            refused.raw_os_error(),
            Some(rustix::io::Errno::LOOP.raw_os_error()),
            "expected ELOOP, got {refused:?}"
        );
        assert_eq!(
            std::fs::read(&victim).expect("victim"),
            b"SENTINEL",
            "the victim file was opened through the planted leaf"
        );
    }

    /// A self-owned symlink in a PARENT component is followed.
    ///
    /// This is the `/backup -> /mnt/disk` admin layout: ownership, not location,
    /// is the trust signal, so the walk must not refuse the operator's own
    /// indirection. Pairs with the leaf cell above - together they show the two
    /// halves are genuinely different policies rather than one blanket rule.
    #[test]
    fn follows_a_self_owned_parent_symlink() {
        let temp = TempDir::new().expect("tempdir");
        let real = temp.path().join("real");
        std::fs::create_dir(&real).expect("mkdir");
        let via = temp.path().join("via");
        symlink(&real, &via).expect("symlink");

        let mut created =
            operator_open_append(&via.join("rsync.log"), 0o600).expect("append through parent");
        created.write_all(b"line\n").expect("write");

        assert_eq!(
            std::fs::read(real.join("rsync.log")).expect("log"),
            b"line\n",
            "the log did not land in the directory the trusted parent pointed at"
        );
    }

    /// Append creates the file when absent and does not truncate it when present.
    #[test]
    fn append_creates_then_appends() {
        let temp = TempDir::new().expect("tempdir");
        let log = temp.path().join("rsync.log");

        operator_open_append(&log, 0o600)
            .expect("create")
            .write_all(b"one\n")
            .expect("write");
        operator_open_append(&log, 0o600)
            .expect("reopen")
            .write_all(b"two\n")
            .expect("write");

        assert_eq!(std::fs::read(&log).expect("log"), b"one\ntwo\n");
    }
}
