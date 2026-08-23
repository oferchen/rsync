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
use rustix::io::Errno;

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

/// The components a walk must step through, in order.
///
/// `/` is expressed by the walk starting at the root and `.` is a no-op, so
/// both drop out. `..` is kept as a literal component: the walk spends it as
/// movement against the directory it has actually reached, which is not the
/// same as collapsing it textually. A Windows prefix cannot occur on a
/// Unix-only module.
fn walk_components(path: &Path) -> Vec<OsString> {
    path.components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_os_string()),
            Component::ParentDir => Some(OsString::from("..")),
            Component::RootDir | Component::CurDir | Component::Prefix(_) => None,
        })
        .collect()
}

/// Push the components of `path` onto the front of `pending`, in order.
fn prepend_components(pending: &mut Vec<OsString>, path: &Path) {
    let mut head = walk_components(path);
    head.append(pending);
    *pending = head;
}

/// Open the walk's starting directory: `/` for an absolute path, `.` otherwise.
fn open_start_dir(absolute: bool) -> io::Result<OwnedFd> {
    rustix::fs::open(
        if absolute { "/" } else { "." },
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
}

/// Open the walk's final component with the caller's flags.
///
/// `O_NOFOLLOW` is added unconditionally, but by this point the component is
/// already known not to be a symlink - the walk resolved that above. It is a
/// race backstop, closing the window between the `statat` and this `openat`,
/// not the leaf policy.
///
/// upstream: `rsync-3.5.0/syscall.c:469`.
fn open_final(
    dirfd: BorrowedFd<'_>,
    name: &OsStr,
    flags: OFlags,
    mode: Mode,
) -> io::Result<OwnedFd> {
    rustix::fs::openat(
        dirfd,
        name,
        flags | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        mode,
    )
    .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()))
}

/// Walk `path` component-by-component and open its final component.
///
/// This is the whole resolver. Every component - **parents and leaf alike** -
/// is inspected with `AT_SYMLINK_NOFOLLOW` and then classified by one rule: a
/// symlink owned by uid 0 or the euid is the operator's own layout and is
/// followed (its target spliced into the remaining path, an absolute target
/// restarting the walk at `/`), and a symlink owned by anyone else is refused.
/// Only a component that is *not* a symlink is opened.
///
/// The leaf is not a special case, and that is the point. Upstream applies the
/// ownership test to `is_last` exactly as it does to a parent - see the
/// contract at `syscall.c:270-272`: "refusing to traverse any symlink (parent
/// or leaf) not owned by uid 0 or our euid. A trusted-owned symlink (e.g.
/// root's `/var/log -> /data/log`) is still followed; an untrusted one fails
/// `ELOOP`." Refusing every leaf symlink instead would break the operator's own
/// `/var/log -> /data/log`, which is the ordinary case this resolver exists to
/// keep working.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:286` `ona_open()` - this walk. The loop at `:367`,
///   the `O_CREAT` leaf arm at `:381-396`, the ownership test at `:406`, the
///   splice at `:421-455`, the `is_last` open at `:460-469`, and the
///   `S_ISDIR`/`ENOTDIR` guard on interior components at `:479`.
/// - `rsync-3.5.0/syscall.c:537` `open_no_attacker_symlinks()` - `ona_open` on
///   the full path, which is [`operator_open_with`].
/// - `rsync-3.5.0/syscall.c:558` `owner_walk_parent()` - the same `ona_open` on
///   the *parent directory*, which is [`owner_trusted_parent`]. One walk, two
///   entry points; the difference is only what path each hands it.
///
/// # Errors
///
/// - `ELOOP` when a component is a symlink owned by an untrusted uid, or the
///   hop budget is exhausted. This is the security refusal, and it is
///   deliberately not `EXDEV`: callers treat `EXDEV` as cross-device and fall
///   back to copy+remove, which would defeat the refusal.
/// - `ENOTDIR` when an interior component is not a directory.
/// - Otherwise the `openat`/`statat`/`readlinkat` errno verbatim.
fn owner_walk_open(path: &Path, flags: OFlags, mode: Mode) -> io::Result<OwnedFd> {
    // upstream: syscall.c:300-302 - "Opted out (local --insecure-links, or a
    // daemon module with `insecure links = yes`): restore the legacy
    // symlink-following open." The test sits at the top of ona_open(), so it
    // covers open_no_attacker_symlinks() and owner_walk_parent() alike; the
    // single short-circuit here covers this walk's two entry points for the
    // same reason. Opting out is not "confine less" - it is the pre-3.4.3
    // resolver verbatim, which is what the option promises.
    if crate::confinement::session_optout_allowed() {
        // `owner_trusted_parent` hands the walk an EMPTY parent for a bare
        // relative leaf ("rsync.log"), which the walk below resolves by
        // starting at ".". A plain open() has no such convention and would
        // report ENOENT, so the same meaning has to be spelled out here.
        let target = if path.as_os_str().is_empty() {
            Path::new(".")
        } else {
            path
        };
        return rustix::fs::open(target, flags, mode)
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()));
    }

    let mut pending = walk_components(path);
    let mut dirfd = open_start_dir(path.is_absolute())?;
    let mut hops = MAX_SYMLINK_HOPS;

    while !pending.is_empty() {
        let name = pending.remove(0);
        let is_last = pending.is_empty();

        let stat =
            match rustix::fs::statat(dirfd.as_fd(), name.as_os_str(), AtFlags::SYMLINK_NOFOLLOW) {
                Ok(stat) => stat,
                // upstream: syscall.c:381-396 - the leaf may legitimately not exist
                // yet under O_CREAT (the `--log-file=/tmp/dir/rsync.log` shape).
                // Open it with O_NOFOLLOW so a leaf raced into a symlink between
                // this failed stat and the open is still refused.
                Err(errno)
                    if is_last && errno == Errno::NOENT && flags.contains(OFlags::CREATE) =>
                {
                    return open_final(dirfd.as_fd(), name.as_os_str(), flags, mode);
                }
                Err(errno) => return Err(io::Error::from_raw_os_error(errno.raw_os_error())),
            };

        if FileType::from_raw_mode(stat.st_mode as _) == FileType::Symlink {
            // upstream: syscall.c:406 - an other-uid symlink is the attacker's
            // and is refused; uid 0 or our own euid is the operator's own
            // layout and is followed. This arm is reached for the leaf too.
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
                // upstream: syscall.c:445 "followed an absolute target: restart from /".
                dirfd = open_start_dir(true)?;
            }
            prepend_components(&mut pending, &target);
            continue;
        }

        if is_last {
            return open_final(dirfd.as_fd(), name.as_os_str(), flags, mode);
        }

        // upstream: syscall.c:479 - an interior component that is not a
        // directory is ENOTDIR, not a silent stop.
        if FileType::from_raw_mode(stat.st_mode as _) != FileType::Directory {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        dirfd = open_dir_component(dirfd.as_fd(), name.as_os_str())?;
    }

    // No components at all (`/`, `.`, or empty): the start directory is itself
    // the answer. Reached via `owner_trusted_parent` for a bare relative leaf
    // such as `rsync.log`, whose parent is "" and whose anchor is therefore ".".
    Ok(dirfd)
}

/// Open the parent directory of `path` via the ownership walk.
///
/// Returns the parent descriptor plus the final component, ready for a `*at`
/// operation. The leaf is deliberately *not* resolved here: a rename or link
/// targets the name itself, so replacing a symlink sitting at that name is the
/// correct outcome, not something to refuse.
///
/// upstream: `rsync-3.5.0/syscall.c:558` `owner_walk_parent()` - the same walk
/// as [`owner_walk_open`], handed the parent directory instead of the path.
///
/// # Errors
///
/// - `EINVAL` when `path` has no final component (`/`, `.`, `..`, or empty),
///   which no operator path being renamed onto can have.
/// - Otherwise see [`owner_walk_open`].
pub fn owner_trusted_parent(path: &Path) -> io::Result<(OwnedFd, OsString)> {
    let Some(leaf) = path.file_name().map(OsStr::to_os_string) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let dirfd = owner_walk_open(parent, OFlags::RDONLY | OFlags::DIRECTORY, Mode::empty())?;
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

/// Open `path` with every component resolved by the ownership walk.
///
/// One policy applies end to end, leaf included: follow a symlink owned by uid
/// 0 or our euid, refuse any other-uid one. Both directions matter here.
///
/// - It **defends** the `/tmp/attackerdir/rsync.log` shape, where a parent is
///   flipped to a symlink and the leaf does not exist yet - so there is nothing
///   for a leaf-only `O_NOFOLLOW` to reject.
/// - It **permits** the operator's own `/var/log -> /data/log`. Refusing every
///   leaf symlink would break the ordinary administrative layout this resolver
///   exists to keep working, which is why upstream tests it
///   (`operator-path-log-file`, the SAME-UID abs-leaf cell).
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/syscall.c:270-272` - the contract in upstream's own words:
///   "refusing to traverse any symlink (parent or leaf) not owned by uid 0 or
///   our euid. A trusted-owned symlink (e.g. root's `/var/log -> /data/log`) is
///   still followed; an untrusted one fails `ELOOP`."
/// - `rsync-3.5.0/syscall.c:537` `open_no_attacker_symlinks()` - the public
///   entry point this mirrors, used for the opens that are not confined beneath
///   a root (`--log-file`, `--*-from`, lock/motd - `syscall.c:232`).
///
/// # Errors
///
/// - `EINVAL` when `path` has no final component.
/// - Otherwise see [`owner_walk_open`].
fn operator_open_with(path: &Path, flags: OFlags, mode: Mode) -> io::Result<std::fs::File> {
    // `/`, `.` and `..` name a directory, never a file to open; the walk would
    // otherwise hand back its own anchor.
    if path.file_name().is_none() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    owner_walk_open(path, flags, mode).map(std::fs::File::from)
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

/// Open an operator-supplied path read/write, creating it if absent.
///
/// The daemon `lock file` shape: the file is both read and written in place
/// (upstream locks byte ranges in it, oc's Windows arm rewrites a count map),
/// and the daemon creates it on first use as root, before the privilege drop.
/// That makes the `O_CREAT` the dangerous part - a symlink planted at any
/// component redirects a privileged create-and-write to a file the operator
/// never named, and the leaf does not exist yet for an `O_NOFOLLOW` to reject.
///
/// `mode` applies only when the file is created, as with `open(2)`.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/connection.c:35` `claim_connection()` -
///   `open_no_attacker_symlinks(fname, O_RDWR|O_CREAT, 0600)`.
///
/// # Errors
///
/// See [`operator_open_with`].
pub fn operator_open_rw_create(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    operator_open_with(
        path,
        OFlags::RDWR | OFlags::CREATE,
        // `RawMode` is u16 on macOS and u32 on Linux, so route the cast through
        // it rather than naming either width here.
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
    )
}

/// Open an operator-supplied path for writing, creating or truncating it.
///
/// The batch-file shape: `--write-batch` and its `.sh` companion are named by
/// the operator and created fresh on every run, so the `O_CREAT` is again the
/// dangerous part - the leaf need not exist for a planted symlink at any
/// component to redirect the write.
///
/// `mode` applies only when the file is created, as with `open(2)`. Upstream
/// relies on exactly that: re-running `--write-batch` over an existing batch
/// file truncates it and leaves its mode alone. Forcing the mode afterwards
/// with a `chmod` would both diverge from that and reopen the window this walk
/// closes, so the mode is passed to the create and never re-applied.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/batch.c:263` - the batch file itself:
///   `open_no_attacker_symlinks(batch_name, O_WRONLY|O_CREAT|O_TRUNC|O_BINARY,
///   S_IRUSR|S_IWUSR)` - owner-only, because the batch holds the file data.
/// - `rsync-3.5.0/batch.c:254` - the `.sh` companion, the same call with
///   `S_IRUSR|S_IWUSR|S_IXUSR` so the generated script is executable.
///
/// # Errors
///
/// See [`operator_open_with`].
pub fn operator_open_write_create(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    operator_open_with(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        // `RawMode` is u16 on macOS and u32 on Linux, so route the cast through
        // it rather than naming either width here.
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
    )
}

/// Read an operator-supplied file to a `String` through the ownership walk.
///
/// The `read_to_string` counterpart to [`operator_open_read`], for the auxiliary
/// files rsync consumes whole as text rather than streaming: the daemon config
/// and `secrets file`, `motd`, `--password-file`, and the filter/`--files-from`
/// lists. Reading these with a plain path-based `std::fs::read_to_string` lets a
/// symlink planted at *any* component redirect a privileged read to a file the
/// operator never named.
///
/// Opening and reading is one operation here on purpose: a caller that resolved
/// the path first and read it second would reintroduce exactly the window the
/// walk exists to close.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/params.c:586` - the daemon config file.
/// - `rsync-3.5.0/authenticate.c:159` / `:245` - `secrets file` and
///   `--password-file`.
/// - `rsync-3.5.0/clientserver.c:188` - `motd`.
///
///   Each opens with `open_no_attacker_symlinks()` and reads from the returned
///   descriptor; none of them resolves the path independently first.
///
/// # Errors
///
/// See [`operator_open_with`]. Additionally surfaces any read error, including
/// `InvalidData` when the file is not valid UTF-8.
pub fn operator_read_to_string(path: &Path) -> io::Result<String> {
    use std::io::Read as _;

    let mut file = operator_open_read(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

#[cfg(test)]
mod tests {
    use super::{
        operator_open_append, operator_open_read, operator_read_to_string,
        symlink_owner_is_trusted, trusted_uid,
    };

    /// The whole point of the helper: content comes back, and it comes back
    /// through the walk rather than a path-based read.
    #[test]
    fn operator_read_to_string_returns_the_file_contents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("rsyncd.conf");
        std::fs::write(&path, "[mod]\n    path = /srv\n").expect("write");

        assert_eq!(
            operator_read_to_string(&path).expect("read the operator file"),
            "[mod]\n    path = /srv\n"
        );
    }

    /// A symlink the caller itself owns is the operator's own layout
    /// (`/etc/rsyncd.conf -> /srv/conf/rsyncd.conf`) and must still be
    /// followed - refusing every leaf symlink would break the ordinary
    /// administrative arrangement this resolver exists to preserve.
    ///
    /// The refusing half needs a foreign-owned symlink and therefore root, so
    /// it is pinned at the predicate by
    /// `refuses_an_owner_that_is_neither_root_nor_the_euid`; this cell pins the
    /// half a careless "confine it" change would silently break.
    #[test]
    fn operator_read_to_string_follows_a_self_owned_symlink() {
        let temp = tempfile::tempdir().expect("tempdir");
        let target = temp.path().join("real.conf");
        std::fs::write(&target, "motd line\n").expect("write");

        let link = temp.path().join("link.conf");
        std::os::unix::fs::symlink(&target, &link).expect("symlink");

        assert_eq!(
            operator_read_to_string(&link).expect("follow a euid-owned symlink"),
            "motd line\n"
        );
    }
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

    /// A symlink AT the named path is FOLLOWED when the operator owns it.
    ///
    /// This is the `/var/log -> /data/log` layout, and upstream tests it as
    /// `operator-path-log-file` (the "SAME-UID abs leaf safe" cell). The leaf
    /// takes the same ownership rule as every parent - `syscall.c:270-272`:
    /// "refusing to traverse any symlink (parent or leaf) not owned by uid 0 or
    /// our euid. A trusted-owned symlink [...] is still followed".
    ///
    /// ⚠ This test previously asserted the OPPOSITE - that a self-owned leaf
    /// symlink is refused - reading `syscall.c:469`'s `flags | O_NOFOLLOW` as
    /// the leaf policy. That line sits inside the `/* Non-symlink. */ if
    /// (is_last)` arm and is only reached once the walk has established the
    /// component is not a symlink; it is a race backstop, not a policy. The old
    /// assertion encoded the bug, which is why the suite stayed green while
    /// upstream's cell failed.
    #[test]
    fn follows_a_self_owned_symlink_at_the_leaf() {
        let temp = TempDir::new().expect("tempdir");
        let target = temp.path().join("real.log");
        std::fs::write(&target, b"SENTINEL").expect("write target");
        let named = temp.path().join("log");
        symlink(&target, &named).expect("symlink");

        let mut opened = operator_open_read(&named).expect("a self-owned leaf symlink is followed");
        let mut body = String::new();
        opened.read_to_string(&mut body).expect("read");
        assert_eq!(
            body, "SENTINEL",
            "the walk did not resolve through the operator's own leaf symlink"
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

    /// The non-vacuity companion to [`follows_a_self_owned_symlink_at_the_leaf`].
    ///
    /// That test proves a *trusted* symlink is followed. On its own it would
    /// also pass if the walk simply followed everything, which is the whole
    /// vulnerability. This pins the other half of the same rule: an owner that
    /// is neither uid 0 nor our euid is refused.
    ///
    /// The predicate is exercised directly because the behavioural cell needs a
    /// symlink owned by a *different* uid, and only root can create one -
    /// upstream's own `operator-path-insecure-links-daemon` cell is skipped for
    /// exactly that reason ("requires root to plant a symlink owned by a
    /// non-self uid"). A runtime-skipped test would report a pass having
    /// checked nothing; this checks the rule on every run.
    ///
    /// upstream: `syscall.c:406` `if (lst.st_uid != 0 && lst.st_uid != trusted_uid)`.
    #[test]
    fn refuses_an_owner_that_is_neither_root_nor_the_euid() {
        assert!(symlink_owner_is_trusted(0), "uid 0 is the operator");
        assert!(
            symlink_owner_is_trusted(trusted_uid()),
            "our own euid is the operator"
        );

        // Search a small range for a uid that is provably neither 0 nor the
        // euid, rather than hardcoding one that could collide with whatever uid
        // the suite happens to run as.
        let foreign = (1..=8u32)
            .find(|candidate| *candidate != trusted_uid())
            .expect("at least one of uids 1..=8 differs from the euid");
        assert!(
            !symlink_owner_is_trusted(foreign),
            "uid {foreign} is neither root nor the euid and must be refused"
        );
    }
}
