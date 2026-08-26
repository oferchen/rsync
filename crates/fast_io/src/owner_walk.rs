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

/// Open an interior directory component beneath `dirfd`, refusing a symlink at
/// the leaf.
///
/// The caller has already `statat`'d the component and found it is not a
/// symlink; `O_NOFOLLOW` closes the window between that check and this open, so
/// a component flipped to a symlink in between fails rather than resolves.
///
/// `flags` comes from [`traversal_dir_flags`] - this descriptor is a step, not
/// the answer.
fn open_dir_component(dirfd: BorrowedFd<'_>, name: &OsStr, flags: OFlags) -> io::Result<OwnedFd> {
    rustix::fs::openat(dirfd, name, flags | OFlags::NOFOLLOW, Mode::empty())
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

/// The absolute path the walk has actually resolved so far, for the
/// confinement refusal.
///
/// Ownership alone cannot bound a peer-driven path: a trusted-owned symlink is
/// FOLLOWED by design (`syscall.c:406`), so it can redirect the resolved target
/// out of the tree. Tracking where the walk has really arrived is what lets the
/// leaf be judged against the confinement root.
///
/// [`Disabled`](AbsPathTracker::Disabled) is not an optimisation but the
/// contract: an [`Ancillary`](crate::confinement::PathKind::Ancillary) open, or
/// a session with no root, has nothing to be outside of, and upstream's own
/// check returns 0 for both (`syscall.c:216`, `syscall.c:239`).
///
/// upstream: `rsync-3.5.0/syscall.c:245` `abspath_step()` and the `abspath`
/// state it advances (`syscall.c:304-329`).
enum AbsPathTracker {
    Disabled,
    Tracking { abspath: PathBuf },
}

impl AbsPathTracker {
    /// Seed the tracker for a walk of `path`.
    ///
    /// An absolute path starts at `/`. A relative one starts where the walk
    /// itself does, which is the process's physical working directory - read,
    /// not assumed. Upstream shortcuts the daemon arm to `module_dir` because
    /// it knows a daemon's cwd IS the module root (`syscall.c:308-310`);
    /// reading the real cwd agrees with that whenever the assumption holds and
    /// is right when it does not, which is why it is used for both arms. It is
    /// the PHYSICAL cwd, as upstream's own comment requires: a lexical name
    /// would sit at a different depth after descending a trusted symlink, and a
    /// `..` that really escapes would look like it landed inside.
    fn start(path: &Path, kind: crate::confinement::PathKind) -> io::Result<Self> {
        if kind != crate::confinement::PathKind::Confined
            || crate::confinement::session_confinement_root().is_none()
        {
            return Ok(Self::Disabled);
        }
        let abspath = if path.is_absolute() {
            PathBuf::from("/")
        } else {
            std::env::current_dir()?
        };
        Ok(Self::Tracking { abspath })
    }

    /// Advance by one resolved component, normalising `.` and `..` exactly as
    /// `openat` does so the check sees the REAL resolved target.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:245` `abspath_step()`.
    fn step(&mut self, name: &OsStr) {
        let Self::Tracking { abspath } = self else {
            return;
        };
        if name == OsStr::new(".") {
            return;
        }
        if name == OsStr::new("..") {
            // `pop` at the root is a no-op, which is what `/..` resolves to.
            abspath.pop();
            return;
        }
        abspath.push(name);
    }

    /// A followed absolute symlink target restarts resolution at `/`.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:445`.
    fn restart_at_root(&mut self) {
        if let Self::Tracking { abspath } = self {
            *abspath = PathBuf::from("/");
        }
    }

    /// Refuse with `ELOOP` when the resolved path has left the confinement
    /// root.
    ///
    /// `ELOOP` and deliberately not `EXDEV`: callers treat `EXDEV` as
    /// cross-device and fall back to copy+remove, which would launder the
    /// refusal.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:464-466`.
    fn refuse_if_outside(&self) -> io::Result<()> {
        let Self::Tracking { abspath } = self else {
            return Ok(());
        };
        if crate::confinement::outside_session_root(abspath, crate::confinement::PathKind::Confined)
        {
            return Err(io::Error::from_raw_os_error(libc::ELOOP));
        }
        Ok(())
    }
}

/// Flags for a dirfd the walk only *steps through*, never hands back.
///
/// # The divergence, and why it is not a policy change
///
/// Upstream opens intermediates `O_RDONLY|O_DIRECTORY` (`syscall.c:493`). oc
/// opens them `O_PATH` on Linux. That is a deliberate divergence, forced by a
/// difference in the CONFINEMENT MECHANISM, not by a difference in policy:
///
/// - Upstream confines a daemon with **chroot**, which RELOCATES `/`. Its walk
///   therefore starts inside the jail and every component it opens is reachable
///   by construction.
/// - oc confines with **Landlock**, which does NOT relocate `/`. The identical
///   walk from `/` must open `/`, `/tmp`, `/tmp/.tmpXXXX`, ... - none of which
///   the ruleset grants. In Landlock's model `openat(O_RDONLY|O_DIRECTORY)` is
///   an ACCESS and needs `LANDLOCK_ACCESS_FS_READ_DIR`; `O_PATH` names a
///   location and needs nothing. So the walk cannot TRAVERSE, and dies before
///   reaching a leaf that is plainly inside the granted tree.
///
/// `O_PATH` is the minimum privilege traversal actually requires - POSIX
/// traversal needs `x`, not `r`. The collision is between two oc-side
/// mechanisms, not between oc and upstream.
///
/// # What is preserved
///
/// EVERY CHECK is byte-identical; only the intermediate open MODE changes:
/// the per-component `statat(AT_SYMLINK_NOFOLLOW)`, the
/// [`symlink_owner_is_trusted`] test and its `ELOOP`, the hop budget, the
/// interior-non-directory `ENOTDIR`, the confinement predicate at the leaf, and
/// the leaf itself opened with the CALLER's flags. Pinned by
/// `every_check_survives_the_o_path_traversal`.
///
/// ⚠ `O_PATH`'s exemption is a kernel property, not a promise - measured
/// permissive on Linux 7.1.5. A kernel that began governing `O_PATH` opens
/// under Landlock would refuse these exactly as it refuses `O_RDONLY` today,
/// which fails CLOSED: the walk stops and nothing escapes.
#[cfg(target_os = "linux")]
fn traversal_dir_flags() -> OFlags {
    OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC
}

/// Non-Linux has no Landlock, so the walk keeps upstream's own flags.
///
/// upstream: `rsync-3.5.0/syscall.c:493`.
#[cfg(not(target_os = "linux"))]
fn traversal_dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC
}

/// Whether the walk's anchor must be reopened before it is handed back.
///
/// True exactly where [`traversal_dir_flags`] returns an `O_PATH` descriptor,
/// which names a location rather than being a working directory handle.
const fn traversal_is_by_location() -> bool {
    cfg!(target_os = "linux")
}

/// Open the walk's starting directory: `/` for an absolute path, `.` otherwise.
///
/// `flags` comes from [`traversal_dir_flags`]; upstream opens the same two
/// directories with the same meaning at `syscall.c:349-351`.
fn open_start_dir(absolute: bool, flags: OFlags) -> io::Result<OwnedFd> {
    rustix::fs::open(if absolute { "/" } else { "." }, flags, Mode::empty())
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
///   the full path, which is `operator_open_with`.
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
fn owner_walk_open(
    path: &Path,
    flags: OFlags,
    mode: Mode,
    kind: crate::confinement::PathKind,
) -> io::Result<OwnedFd> {
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
    let by_location = traversal_is_by_location();
    let traverse = traversal_dir_flags();
    let mut dirfd = open_start_dir(path.is_absolute(), traverse)?;
    let mut hops = MAX_SYMLINK_HOPS;
    let mut tracker = AbsPathTracker::start(path, kind)?;

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
                    // upstream: syscall.c:387-391 - the confinement is checked on
                    // this arm too. A create is exactly where a redirected leaf
                    // does its damage, so skipping it here would leave the
                    // `--partial-dir`/`--temp-dir` shapes unconfined.
                    tracker.step(&name);
                    tracker.refuse_if_outside()?;
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
                dirfd = open_start_dir(true, traverse)?;
                tracker.restart_at_root();
            }
            prepend_components(&mut pending, &target);
            continue;
        }

        if is_last {
            // upstream: syscall.c:460-468 - `abspath_step()` then
            // `abspath_outside_confinement()`. The check belongs HERE and not on
            // interior components: an absolute walk passes through the root's
            // own ancestors on the way down, which are not-yet-arrived rather
            // than diverged.
            tracker.step(&name);
            tracker.refuse_if_outside()?;
            return open_final(dirfd.as_fd(), name.as_os_str(), flags, mode);
        }
        tracker.step(&name);

        // upstream: syscall.c:479 - an interior component that is not a
        // directory is ENOTDIR, not a silent stop.
        if FileType::from_raw_mode(stat.st_mode as _) != FileType::Directory {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        dirfd = open_dir_component(dirfd.as_fd(), name.as_os_str(), traverse)?;
    }

    // No components at all (`/`, `.`, or empty): the start directory is itself
    // the answer. Reached via `owner_trusted_parent` for a bare relative leaf
    // such as `rsync.log`, whose parent is "" and whose anchor is therefore ".".
    //
    // The anchor is the RESULT on this arm rather than a step, so it has to
    // carry the caller's flags: `traversal_dir_flags` may have opened it
    // `O_PATH`, which names a location and is not a working descriptor. The
    // reopen is the same open the caller would have got before, and it is
    // subject to the sandbox exactly as that one was.
    if by_location {
        return rustix::fs::openat(dirfd.as_fd(), ".", flags | OFlags::CLOEXEC, mode)
            .map_err(|errno| io::Error::from_raw_os_error(errno.raw_os_error()));
    }
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
/// as `owner_walk_open`, handed the parent directory instead of the path.
///
/// # Errors
///
/// - `EINVAL` when `path` has no final component (`/`, `.`, `..`, or empty),
///   which no operator path being renamed onto can have.
/// - Otherwise see `owner_walk_open`.
pub fn owner_trusted_parent(path: &Path) -> io::Result<(OwnedFd, OsString)> {
    let Some(leaf) = path.file_name().map(OsStr::to_os_string) else {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    };
    let parent = path.parent().unwrap_or_else(|| Path::new(""));
    let dirfd = owner_walk_open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY,
        Mode::empty(),
        crate::confinement::PathKind::Ancillary,
    )?;
    Ok((dirfd, leaf))
}

/// Create a directory at an operator-supplied path through the ownership walk.
///
/// The `mkdir` counterpart to the `operator_open_*` family: the parent chain is
/// resolved by the ownership walk and the leaf created with `mkdirat` on the
/// resulting descriptor, so a symlink planted at any component by a foreign uid
/// cannot redirect the new directory out of the tree. An existing directory at
/// the leaf is success, which makes the call idempotent across runs that reuse a
/// reserved `--partial-dir`.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/util1.c:1501` `handle_partial_dir()` - sets
///   `operator_path_resolve = 1` around its `do_lstat_at()`/`do_mkdir_at(dir,
///   0700)` pair precisely so the partial dir is created through the ownership
///   walk rather than by a plain path-based `mkdir`.
///
/// # Errors
///
/// Surfaces any walk error (including the refusal of a foreign-owned symlink)
/// and any `mkdirat` error other than `EEXIST`.
pub fn operator_mkdir(path: &Path, mode: u32) -> io::Result<()> {
    let (parent, leaf) = owner_trusted_parent(path)?;
    match rustix::fs::mkdirat(
        &parent,
        leaf.as_os_str(),
        // `RawMode` is u16 on macOS and u32 on Linux, so route the cast through
        // it rather than naming either width here.
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
    ) {
        Ok(()) => Ok(()),
        Err(Errno::EXIST) => Ok(()),
        Err(error) => Err(error.into()),
    }
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
/// - Otherwise see `owner_walk_open`.
fn operator_open_with(path: &Path, flags: OFlags, mode: Mode) -> io::Result<std::fs::File> {
    operator_open_kind(path, flags, mode, crate::confinement::PathKind::Ancillary)
}

/// `operator_open_with` with the caller's [`PathKind`](crate::confinement::PathKind).
///
/// Upstream expresses the same distinction as a global toggled around each call
/// site; passing it makes an unstated answer a compile error instead of a
/// control-flow accident.
fn operator_open_kind(
    path: &Path,
    flags: OFlags,
    mode: Mode,
    kind: crate::confinement::PathKind,
) -> io::Result<std::fs::File> {
    // `/`, `.` and `..` name a directory, never a file to open; the walk would
    // otherwise hand back its own anchor.
    if path.file_name().is_none() {
        return Err(io::Error::from_raw_os_error(libc::EINVAL));
    }
    owner_walk_open(path, flags, mode, kind).map(std::fs::File::from)
}

/// Open an operator-supplied path read-only through the ownership walk.
///
/// The read-side counterpart for the operator paths rsync only consumes -
/// `--password-file`, `--exclude-from`/`--include-from`/`--files-from`, the
/// daemon config and secrets files.
///
/// # Errors
///
/// See `operator_open_with`.
pub fn operator_open_read(path: &Path) -> io::Result<std::fs::File> {
    operator_open_with(path, OFlags::RDONLY, Mode::empty())
}

/// Open a filter/merge file read-only through the ownership walk, additionally
/// bound to the session's confinement root.
///
/// The ownership walk on its own is not enough for a peer-driven merge file: a
/// non-chrooted daemon writes `--backup-dir` entries as root, so a raced backup
/// symlink is ROOT-owned - exactly what the walk treats as trusted - and naming
/// it in a dir-merge rule would read an out-of-module file in as filter rules,
/// whose text comes back to the peer in "Unknown filter rule" errors. The root
/// check after the follow is what closes that.
///
/// The daemon's OWN `filter` / `include from` / `exclude from` parameters are
/// deliberately NOT opened through this: they are operator-configured and
/// legitimately live outside the module (`/etc/rsync/excludes` and the like).
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/exclude.c:1668-1684` `parse_filter_file()` - the comment this
///   paraphrases, and the `if (!daemon_config_filter_file)
///   operator_path_resolve = 1;` that scopes it.
/// - `rsync-3.5.0/syscall.c:186-240` `abspath_outside_confinement()`.
///
/// # Errors
///
/// - `ELOOP` when a component is an untrusted-owner symlink, or when the
///   resolved path lands outside the confinement root.
/// - Otherwise see `operator_open_with`.
pub fn operator_open_read_confined(path: &Path) -> io::Result<std::fs::File> {
    operator_open_kind(
        path,
        OFlags::RDONLY,
        Mode::empty(),
        crate::confinement::PathKind::Confined,
    )
}

/// Open an operator-supplied path for appending, creating it if absent.
///
/// This is the `--log-file` shape, and the reason the leaf `O_NOFOLLOW` matters
/// as much as the walk: a privileged rsync appending through a planted symlink
/// writes attacker-chosen bytes into an attacker-chosen file.
///
/// # Errors
///
/// See `operator_open_with`.
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
/// See `operator_open_with`.
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
/// See `operator_open_with`.
pub fn operator_open_write_create(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    operator_open_with(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        // `RawMode` is u16 on macOS and u32 on Linux, so route the cast through
        // it rather than naming either width here.
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
    )
}

/// Open an operator-supplied path for writing, creating or truncating it,
/// additionally bound to the session's confinement root.
///
/// The `--backup-dir` shape under `--inplace`. The in-place backup has to
/// duplicate the destination's pre-transfer bytes into the operator-named
/// backup path, and the ownership walk follows a trusted-owned symlink at that
/// path by design. A non-chrooted daemon writes its `--backup-dir` entries as
/// its own uid, so a backup entry is TRUSTED-owned by construction: point one
/// outside the module and the follow carries an in-module file's contents out
/// of the tree. Ownership decides whether to follow; the root decides whether
/// the landing site is acceptable.
///
/// The `Ancillary` twin [`operator_open_write_create`] stays with the opens
/// that may legitimately live outside the tree - the `--write-batch` files.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/backup.c:443-449` `make_backup()` - `operator_path_resolve =
///   1` around the whole backup, naming `--backup-dir` as an operator path.
/// - `rsync-3.5.0/generator.c:2281-2301` and `:2327-2349` - the in-place backup
///   bypasses `make_backup()`, so the generator raises the same flag around its
///   own `copy_file()` / `do_open_at()`.
/// - `rsync-3.5.0/syscall.c:186-240` `abspath_outside_confinement()`.
///
/// # Errors
///
/// - `ELOOP` when a component is an untrusted-owner symlink, or when the
///   resolved path lands outside the confinement root.
/// - Otherwise see `operator_open_with`.
pub fn operator_open_write_create_confined(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    operator_open_kind(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
        crate::confinement::PathKind::Confined,
    )
}

/// Exclusively create an operator-supplied path through the ownership walk.
///
/// The `O_EXCL` twin of [`operator_open_write_create`], for the receiver's
/// staging temp under an operator-supplied `--temp-dir`.
///
/// `O_EXCL` alone is NOT sufficient here. It refuses a symlink at the *final*
/// component only, so it defeats a planted `.name.XXXXXX` but not a planted
/// `--temp-dir` itself: the path resolves *through* the directory symlink
/// before the leaf is ever considered, and the receiver's scratch file - and
/// with it the transferred data - lands outside the tree.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/receiver.c:426-434` `open_tmpfile()` - for any non-chrooted
///   receiver, `secure_mkstemp(fnametmp, mode, tmpdir != NULL)`. The third
///   argument is the resolver selector, and upstream's own comment states the
///   rule: "An operator-supplied --temp-dir (tmpdir) gets the ownership-walk
///   resolver (it may legitimately point outside the tree); the deep-entry-dir
///   fallback ... gets the strict transfer-path one."
///
/// Name generation stays the caller's: upstream's `mkstemp` is
/// generate-then-`O_EXCL`-retry, which is what the caller's retry loop already
/// implements. This supplies only the walked, exclusive open.
///
/// # Errors
///
/// See [`operator_open_with`]. `AlreadyExists` is the expected, retryable
/// outcome when the generated name collides.
pub fn operator_open_create_new(path: &Path, mode: u32) -> io::Result<std::fs::File> {
    operator_open_with(
        path,
        OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL,
        // `RawMode` is u16 on macOS and u32 on Linux, so route the cast through
        // it rather than naming either width here.
        Mode::from_bits_truncate(mode as rustix::fs::RawMode),
    )
}
/// Open an operator-supplied path as a receiver output through the ownership
/// walk.
///
/// The `--partial-dir` staging target of a `one_inplace` update: the receiver
/// writes the file data straight into the operator-named partial directory, so
/// a symlink planted at any component redirects the peer's payload to a file
/// the operator never named.
///
/// `create` and `truncate` carry `O_CREAT` and `O_TRUNC` respectively, matching
/// the flags the caller would pass to `open(2)`; `mode` applies only when the
/// file is created.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/receiver.c:1204-1206` - `secure_recv_open(fnametmp,
///   O_WRONLY|O_CREAT, 0600, one_inplace)`, the primary arm.
/// - `rsync-3.5.0/receiver.c:1212-1214` - the same call without `O_CREAT`, the
///   `protected_regular` retry. Upstream threads `one_inplace` through both, so
///   a retry cannot silently drop to the plain resolver.
///
/// `O_TRUNC` never appears at either upstream site: a staged partial IS the
/// delta basis, and upstream sizes the result with a final `ftruncate`. The
/// parameter exists so the chain's contract stays the caller's to state.
///
/// # Errors
///
/// See [`operator_open_with`].
pub fn operator_open_recv(
    path: &Path,
    create: bool,
    truncate: bool,
    mode: u32,
) -> io::Result<std::fs::File> {
    let mut flags = OFlags::WRONLY;
    if create {
        flags |= OFlags::CREATE;
    }
    if truncate {
        flags |= OFlags::TRUNC;
    }
    operator_open_with(
        path,
        flags,
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
/// See `operator_open_with`. Additionally surfaces any read error, including
/// `InvalidData` when the file is not valid UTF-8.
pub fn operator_read_to_string(path: &Path) -> io::Result<String> {
    use std::io::Read as _;

    let mut file = operator_open_read(path)?;
    let mut contents = String::new();
    file.read_to_string(&mut contents)?;
    Ok(contents)
}

/// `lstat`s an operator-supplied path with its parent resolved by the ownership
/// walk, so a foreign-owned symlink among the parent components cannot redirect
/// the stat.
///
/// Only the parent is walked. The leaf is `fstatat`'d with
/// `AT_SYMLINK_NOFOLLOW`, never opened and never resolved: a symlink sitting at
/// the leaf must report *as a symlink* so a caller's file-type filter can drop
/// it, even when the link is trusted-owned. Opening the leaf instead would both
/// follow that symlink and demand read permission the caller may not need.
///
/// The returned [`std::fs::Metadata`] comes from a second, path-based stat, so
/// callers keep the `std` type their comparisons are written against. It is
/// accepted only when it names the same `(dev, ino)` the confined stat saw: a
/// parent flipped between the two calls lands on a different inode and is
/// refused, which is what makes the path stat safe to use as the carrier.
///
/// Callers are expected to treat any error as "nothing here" - upstream's
/// `basis_link_stat()` returns `-1` on a refused walk, and every one of its call
/// sites turns that into a `continue`, so a redirected basis simply looks absent
/// and the file transfers normally.
///
/// # Upstream Reference
///
/// - `rsync-3.5.0/generator.c:962` `basis_link_stat()` - `owner_walk_parent()`
///   for the parent components, then `link_stat_at()` on the leaf through the
///   returned descriptor.
/// - `rsync-3.5.0/generator.c:1084` / `:1110` / `:1227` / `:1254` - the call
///   sites, each treating a failure as "no candidate in this basis dir".
///
/// # Errors
///
/// Surfaces the walk's `ELOOP` when a parent component is a symlink owned by
/// neither uid 0 nor our euid, any other error the walk reports (see
/// [`owner_trusted_parent`]), the `fstatat` error for a missing or unreadable
/// leaf, and `io::ErrorKind::NotFound` when the path-based stat disagrees with
/// the confined one about which inode the leaf names.
pub fn operator_symlink_metadata(path: &Path) -> io::Result<std::fs::Metadata> {
    use std::os::unix::fs::MetadataExt as _;

    let (parent, leaf) = owner_trusted_parent(path)?;
    let confined = crate::fstatat_nofollow(parent.as_fd(), &leaf)?;
    let meta = std::fs::symlink_metadata(path)?;
    if meta.dev() == confined.dev() && meta.ino() == confined.ino() {
        Ok(meta)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "operator path changed inode between the confined and path stat",
        ))
    }
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
    use super::operator_symlink_metadata;
    use std::io::{Read, Write};
    use std::os::unix::fs::symlink;
    use tempfile::TempDir;

    /// The basis lookup must see a LEAF symlink *as a symlink*, so a caller's
    /// `is_file()` filter drops it - even though the same link would be followed
    /// were it a parent component. That asymmetry is deliberate: upstream reaches
    /// the leaf through `link_stat_at(.., 0)` after walking only the parent
    /// (`generator.c:979-990`), which is what stops a symlinked `--link-dest`
    /// entry from being hard-linked or read.
    #[test]
    fn operator_symlink_metadata_does_not_follow_a_leaf_symlink() {
        let temp = TempDir::new().expect("tempdir");
        let target = temp.path().join("real");
        std::fs::write(&target, b"basis payload").expect("write");

        let link = temp.path().join("basis");
        symlink(&target, &link).expect("symlink");

        let meta = operator_symlink_metadata(&link).expect("stat the leaf symlink");
        assert!(
            meta.file_type().is_symlink(),
            "a leaf symlink must report as a symlink so the is_file() filter drops it"
        );
    }

    /// A PARENT component the operator owns is their own layout
    /// (`--link-dest=/var/backups` where `/var/backups -> /data/backups`) and
    /// must still be followed. Refusing it would make every such basis look
    /// absent and silently re-transfer files upstream hard-links.
    ///
    /// The refusing half needs a symlink owned by neither uid 0 nor our euid and
    /// therefore root, so it is pinned at the predicate by
    /// `refuses_an_owner_that_is_neither_root_nor_the_euid`. This cell pins the
    /// half that a careless "refuse every symlink" change would break.
    #[test]
    fn operator_symlink_metadata_follows_a_self_owned_parent_symlink() {
        let temp = TempDir::new().expect("tempdir");
        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).expect("mkdir");
        std::fs::write(outside.join("basis"), b"basis payload").expect("write");

        let link = temp.path().join("via");
        symlink(&outside, &link).expect("symlink");

        let meta = operator_symlink_metadata(&link.join("basis"))
            .expect("follow a euid-owned parent symlink");
        assert!(meta.file_type().is_file());
        assert_eq!(meta.len(), b"basis payload".len() as u64);
    }

    /// A missing leaf is an error, not a zero-sized success - callers read any
    /// error as "no basis in this directory", and a silent empty result there
    /// would make an absent basis look like a matching one.
    #[test]
    fn operator_symlink_metadata_reports_a_missing_leaf_as_an_error() {
        let temp = TempDir::new().expect("tempdir");
        assert!(operator_symlink_metadata(&temp.path().join("absent")).is_err());
    }

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

    use super::{
        MAX_SYMLINK_HOPS, operator_open_read_confined, operator_open_write_create,
        traversal_dir_flags, traversal_is_by_location,
    };
    use crate::confinement::{Activation, DaemonState, LocalInsecureLinks, Role, install_session};
    use rustix::fs::OFlags;
    use std::path::{Path, PathBuf};

    /// Install a confinement root for the duration of one test.
    ///
    /// The session root is process-global, exactly as upstream's
    /// `confinement_root()` reads process globals. Every test that installs one
    /// therefore relies on nextest's process-per-test model, which the
    /// repository mandates.
    fn confine_to(root: &Path) {
        install_session(&Activation {
            role: Role::Receiver,
            daemon: DaemonState::NotDaemon,
            insecure_links: LocalInsecureLinks::default(),
            confine_root: Some(root.to_path_buf()),
        });
    }

    /// A temp tree with a confinement root, an in-root payload and an
    /// out-of-root one.
    fn confined_fixture() -> (TempDir, PathBuf, PathBuf, PathBuf) {
        let temp = TempDir::new().expect("tempdir");
        let root = temp.path().join("module");
        std::fs::create_dir(&root).expect("mkdir module");
        let inside = root.join("payload");
        std::fs::write(&inside, b"INSIDE").expect("write inside");
        let outside_dir = temp.path().join("outside");
        std::fs::create_dir(&outside_dir).expect("mkdir outside");
        let outside = outside_dir.join("secret");
        std::fs::write(&outside, b"OUTSIDE").expect("write outside");
        (temp, root, inside, outside)
    }

    /// Constraint 1, as an assertion rather than an argument: EVERY CHECK the
    /// walk makes is byte-identical, and only the intermediate open MODE
    /// changes.
    ///
    /// The gate this replaces asserted the weaker "no confinement root =>
    /// traversal flags unchanged". That was conservatism, not security: it
    /// scoped the change by WHO installed a session instead of by WHAT the
    /// change can affect. `O_PATH` governs one thing - how a dirfd the walk
    /// only steps through is opened - and every decision the walk makes is
    /// taken from `statat`/`readlinkat` against that dirfd, none of which
    /// consult it.
    ///
    /// So the invariant is pinned here with NO root installed, where the
    /// tracker is `Disabled` and the confinement check is inert: each of the
    /// remaining checks must still fire, from the same place, for the same
    /// reason.
    #[test]
    fn every_check_survives_the_o_path_traversal() {
        let temp = TempDir::new().expect("tempdir");
        let dir = temp.path().join("d");
        std::fs::create_dir(&dir).expect("mkdir");

        // The leaf takes the CALLER's flags, not the traversal flags - an
        // `O_PATH` leaf could not be read at all, so a correct read is the
        // whole assertion.
        let plain = dir.join("plain");
        std::fs::write(&plain, b"PLAIN").expect("write");
        assert_eq!(
            operator_read_to_string(&plain).expect("a plain absolute path still resolves"),
            "PLAIN",
            "the leaf must still be opened with the caller's flags"
        );

        // syscall.c:479 - an interior component that is not a directory.
        assert_eq!(
            operator_read_to_string(&plain.join("below"))
                .expect_err("an interior regular file is not traversable")
                .raw_os_error(),
            Some(libc::ENOTDIR),
            "the interior-non-directory check must still fire"
        );

        // syscall.c:406 and the hop budget. Every link here is self-owned, so
        // the owner test passes them and only the budget can refuse.
        let mut chain = dir.join("hop0");
        std::os::unix::fs::symlink(&plain, &chain).expect("symlink base");
        for hop in 1..=MAX_SYMLINK_HOPS {
            let next = dir.join(format!("hop{hop}"));
            std::os::unix::fs::symlink(&chain, &next).expect("symlink hop");
            chain = next;
        }
        assert_eq!(
            operator_read_to_string(&chain)
                .expect_err("a chain longer than the budget is refused")
                .raw_os_error(),
            Some(libc::ELOOP),
            "the symlink hop budget must still fire"
        );

        // syscall.c:445 - following an ABSOLUTE target restarts the walk at
        // `/`. This is the arm the per-arm mutation identified as the
        // discriminating one, so it is pinned explicitly rather than left to
        // the general case.
        let via_absolute = dir.join("via_absolute");
        std::os::unix::fs::symlink(&plain, &via_absolute).expect("symlink absolute");
        assert!(
            std::fs::read_link(&via_absolute)
                .expect("readlink")
                .is_absolute(),
            "the fixture must exercise the absolute-restart arm"
        );
        assert_eq!(
            operator_read_to_string(&via_absolute).expect("the restart arm still resolves"),
            "PLAIN"
        );

        // syscall.c:381-396 - a missing leaf under O_CREAT is still created.
        let created = dir.join("created");
        operator_open_write_create(&created, 0o600)
            .expect("O_CREAT leaf")
            .write_all(b"NEW")
            .expect("write");
        assert_eq!(std::fs::read(&created).expect("read"), b"NEW");
    }

    /// The one thing that DOES change: an intermediate dirfd is opened by
    /// location on Linux, and with upstream's own flags everywhere else.
    ///
    /// Unconditional - there is no gate to assert around. Landlock does not
    /// relocate `/` the way upstream's chroot does, so a walk from `/` must
    /// step through components no ruleset grants; `O_PATH` needs no access
    /// right, `O_RDONLY|O_DIRECTORY` needs `READ_DIR`.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:493` - the deliberate divergence.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_intermediate_dirfd_is_opened_by_location_on_linux() {
        let flags = traversal_dir_flags();
        assert!(
            flags.contains(OFlags::PATH),
            "Landlock governs READ_DIR, so a step must not request read access"
        );
        // `O_RDONLY` is 0 on Linux, so `contains(RDONLY)` is vacuously true
        // and cannot express "does not request read access". Compare the raw
        // bits instead - this is the assertion that a mutation back to
        // upstream's flags has to fail.
        assert_eq!(
            flags,
            OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
            "the traversal flags are exactly O_PATH|O_DIRECTORY|O_CLOEXEC"
        );
        assert!(
            flags.contains(OFlags::DIRECTORY) && flags.contains(OFlags::CLOEXEC),
            "the traversal is still directory-only and close-on-exec"
        );
        assert!(
            traversal_is_by_location(),
            "an O_PATH anchor is a location and must be reopened before it is returned"
        );
    }

    /// The mirror half: no Landlock exists off Linux, so the walk keeps
    /// upstream's own flags and there is no anchor to reopen.
    ///
    /// upstream: `rsync-3.5.0/syscall.c:493`.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn an_intermediate_dirfd_keeps_upstreams_flags_off_linux() {
        assert_eq!(
            traversal_dir_flags(),
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            "platforms without Landlock keep upstream's own flags"
        );
        assert!(!traversal_is_by_location());
    }

    /// Constraint 2: a leaf outside the confinement root is refused as a
    /// CONFINEMENT decision - `ELOOP` - and never as an incidental `EACCES`.
    ///
    /// This is what makes the traversal change safe to make: the walk now
    /// reaches the leaf, so the refusal has to come from the root check rather
    /// than from a sandbox that happened to stop the walk earlier. An `EACCES`
    /// here would mean the refusal is still accidental.
    ///
    /// upstream: `syscall.c:464-466` - `abspath_outside_confinement()` fails
    /// the open with `ELOOP`.
    #[test]
    fn a_leaf_outside_the_root_is_refused_with_eloop_not_eacces() {
        let (_temp, root, _inside, outside) = confined_fixture();
        confine_to(&root);

        let error = operator_open_read_confined(&outside)
            .expect_err("a leaf outside the confinement root must be refused");
        assert_eq!(
            error.raw_os_error(),
            Some(libc::ELOOP),
            "the refusal must be the confinement decision, not a traversal failure"
        );
    }

    /// Constraint 3: a `..` in the remainder is spent as movement against the
    /// directory the walk actually reached, so a path that climbs back out of
    /// the root is still refused.
    ///
    /// The companion to the cell above: that one escapes by naming an outside
    /// path directly, this one escapes by walking. Both must land on the same
    /// refusal, which is what shows the tracker judges where the open would
    /// REALLY land rather than how the path was spelled.
    #[test]
    fn a_dot_dot_that_climbs_out_of_the_root_is_still_refused() {
        let (_temp, root, _inside, _outside) = confined_fixture();
        confine_to(&root);

        let escape = root.join("..").join("outside").join("secret");
        let error =
            operator_open_read_confined(&escape).expect_err("`..` above the root must be refused");
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
    }

    /// The in-root control for both refusal cells: a leaf plainly inside the
    /// root still resolves.
    ///
    /// Without it the two cells above would also pass if the walk refused
    /// everything, which is the failure mode this whole change exists to
    /// remove.
    #[test]
    fn a_leaf_inside_the_root_still_resolves() {
        let (_temp, root, inside, _outside) = confined_fixture();
        confine_to(&root);

        let mut opened = operator_open_read_confined(&inside).expect("an in-root leaf resolves");
        let mut body = String::new();
        opened.read_to_string(&mut body).expect("read");
        assert_eq!(body, "INSIDE");
    }

    /// Constraint 4, the ancestor pin: an absolute path that is an ANCESTOR of
    /// the confinement root must keep resolving.
    ///
    /// An absolute walk passes through `/`, `/tmp`, `/tmp/xxx`, ... on its way
    /// down to the root, and those components are not-yet-arrived rather than
    /// diverged. Refusing them would refuse every absolute operator path a
    /// confined session ever names - the walk would be unable to reach its own
    /// root. That is precisely why the confinement test is applied at the LEAF
    /// and not per component, and why stepping by location does not need a
    /// beneath-ness test to be safe.
    ///
    /// upstream: `syscall.c:197` `abspath_outside_confinement()` - an ancestor
    /// of the root is not outside it.
    #[test]
    fn an_ancestor_of_the_confinement_root_still_resolves() {
        let temp = TempDir::new().expect("tempdir");
        let parent = temp.path().join("parent");
        let root = parent.join("module");
        std::fs::create_dir_all(&root).expect("mkdir root");
        confine_to(&root);

        operator_open_read_confined(&parent)
            .expect("an ancestor of the root is descending, not escaping");
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
