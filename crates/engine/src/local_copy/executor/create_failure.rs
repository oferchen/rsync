//! Upstream-format reporting for an entry `atomic_create()` could not create.
//!
//! A local copy runs upstream's generator code, so a failed `mknod`,
//! `symlink`, or hard `link` prints the same `rsyserr(FERROR_XFER, ...)` line
//! a network receiver does. The entry is skipped, the run goes on, and the
//! recorded `got_xfer_error` finishes it `RERR_PARTIAL` (23).
//!
//! # Upstream Reference
//!
//! - `generator.c:2490-2493` - `symlink %s -> "%s" failed`
//! - `generator.c:2521-2522` - `mknod %s failed`
//! - `hlink.c:486-487` - `link %s => %s failed`
//! - `log.c:337-338` - `FERROR_XFER` sets `got_xfer_error`

use std::io;
use std::path::{Component, Path};

use crate::local_copy::{CopyContext, upstream_io_error};

/// Reports a failed `mknod` of a FIFO, socket, or device node.
#[cfg(unix)]
pub(crate) fn report_mknod_failure(
    context: &mut CopyContext,
    destination: &Path,
    error: &io::Error,
) {
    report(
        context,
        &format!("mknod {} failed", full_fname(destination)),
        error,
    );
}

/// Reports a failed `symlink`; `target` is the link text that was written.
pub(crate) fn report_symlink_failure(
    context: &mut CopyContext,
    destination: &Path,
    target: &Path,
    error: &io::Error,
) {
    report(
        context,
        &format!(
            "symlink {} -> \"{}\" failed",
            full_fname(destination),
            target.display()
        ),
        error,
    );
}

/// Reports a failed hard `link` of `destination` to its group `leader`.
///
/// Upstream names the leader by its transfer-relative name, unquoted.
pub(crate) fn report_link_failure(
    context: &mut CopyContext,
    destination: &Path,
    leader: &Path,
    error: &io::Error,
) {
    let leader = leader
        .strip_prefix(context.destination_root())
        .unwrap_or(leader);
    report(
        context,
        &format!(
            "link {} => {} failed",
            full_fname(destination),
            slash_path(leader)
        ),
        error,
    );
}

/// `rsyserr(FERROR_XFER, ...)` from the generator: `who_am_i()` is always
/// `generator` for `atomic_create()`.
fn report(context: &mut CopyContext, what: &str, error: &io::Error) {
    eprintln!("rsync: [generator] {what}: {}", upstream_io_error(error));
    context.record_create_error();
}

/// Renders `path` as upstream `full_fname()` does outside a daemon module:
/// double quoted, with a relative name prefixed by the working directory.
///
/// upstream: util1.c:1528 full_fname()
fn full_fname(path: &Path) -> String {
    let name = slash_path(path);
    if path.has_root() {
        return format!("\"{name}\"");
    }
    match std::env::current_dir() {
        Ok(cwd) => format!("\"{}/{name}\"", cwd.display()),
        Err(_) => format!("\"{name}\""),
    }
}

/// Joins `path` with `/`, dropping `.` components as upstream's cleaned
/// flist names never carry them.
fn slash_path(path: &Path) -> String {
    let mut out = String::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::RootDir => out.push('/'),
            other => {
                if !out.is_empty() && !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&other.as_os_str().to_string_lossy());
            }
        }
    }
    out
}
