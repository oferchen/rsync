//! The commit-path operation a receiver failure came from, and how upstream
//! answers it.
//!
//! Upstream's receiver reports each failed stage in its own words and decides
//! by stage, never by errno, whether the file is skipped or the run ends. Both
//! of oc's receivers - the network one in `transfer` and the local-copy
//! executor here - tag the failing [`io::Error`] at its raising site and share
//! [`CommitDisposition`] to make that decision.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use protocol::MessageCode;

/// Which commit-path operation raised a failure, so the receiver's diagnostic
/// names that operation instead of labelling every failure `mkstemp`.
///
/// Upstream emits one message per site, each naming its own operation:
/// `mkstemp %s failed` (`rsync-3.5.1/receiver.c:465-466`),
/// `open %s failed` (`rsync-3.5.1/receiver.c:1242-1244`),
/// `rename %s -> "%s"` and `copy %s -> "%s"` (`rsync-3.5.1/rsync.c:919-921`),
/// `keep_backup failed: %s -> "%s"` (`rsync-3.5.1/backup.c:402-403`),
/// `close failed on %s` (`rsync-3.5.1/receiver.c:1297-1298`) and
/// `Unable to create partial-dir for %s -- discarding %s.`
/// (`rsync-3.5.1/receiver.c:1319-1323`). oc runs
/// these stages behind one `Result`, so the operation identity has to ride on
/// the error to survive the trip back to the reporting thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitOp {
    /// Creating the `.name.XXXXXX` temp file.
    Mkstemp,
    /// Opening the destination itself for an in-place or device write. The
    /// operand is the destination (upstream's `fnametmp` is `fname` there).
    Open,
    /// Moving the pre-image aside under `--backup`.
    Backup,
    /// Renaming the finished temp file onto its destination. The operand is
    /// the temp file and the destination is the rename target.
    Rename,
    /// Copying the finished temp file onto its destination because the rename
    /// crossed devices. Same operands as [`Self::Rename`].
    Copy,
    /// Closing the received file, where a filesystem that defers write-back
    /// reports a failed write. The operand is the file that was written: the
    /// temp file, or the destination itself for an in-place update.
    Close,
    /// Creating the `--partial-dir` (or `--delay-updates`' `.~tmp~`) a
    /// completed file is staged into. The operand is that directory and the
    /// destination is the file's final path.
    PartialDir,
}

impl CommitOp {
    /// Renders upstream's diagnostic for a failure of this operation.
    ///
    /// `name` is the operand already rendered the way `full_fname()` does
    /// (quoted, with any daemon-module suffix); `target` is the second path
    /// the two-operand forms print - the rename/copy destination relative to
    /// the receiver's root, or the backup name. `reason` is the errno text.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:465-466` - `mkstemp %s failed`.
    /// - `receiver.c:1243-1244` - `open %s failed`.
    /// - `rsync.c:919-921` - `"%s %s -> \"%s\""` with `copy` or `rename`.
    /// - `backup.c:402-403` - `keep_backup failed: %s -> "%s"`.
    /// - `receiver.c:1297-1298` - `close failed on %s`.
    /// - `receiver.c:1319-1323` - `Unable to create partial-dir for %s --
    ///   discarding %s.`, a plain `rprintf(FERROR)` naming the file by its
    ///   transfer name (`target`) with no errno. oc stages only a completed
    ///   file, the `--delay-updates` arm, so the second `%s` is always
    ///   `completed file`.
    #[must_use]
    pub fn failure_line(self, name: &str, target: Option<&Path>, reason: &str) -> String {
        match (self, target) {
            (Self::Mkstemp, _) => format!("rsync: [receiver] mkstemp {name} failed: {reason}"),
            (Self::Open, _) => format!("rsync: [receiver] open {name} failed: {reason}"),
            (Self::Close, _) => format!("rsync: [receiver] close failed on {name}: {reason}"),
            (Self::Rename | Self::Copy, target) => format!(
                "rsync: [receiver] {} {name} -> \"{}\": {reason}",
                if self == Self::Copy { "copy" } else { "rename" },
                target.map_or_else(String::new, |t| t.display().to_string())
            ),
            (Self::Backup, Some(backup)) => format!(
                "rsync: [receiver] keep_backup failed: {name} -> \"{}\": {reason}",
                backup.display()
            ),
            (Self::Backup, None) => {
                format!("rsync: [receiver] keep_backup failed: {name}: {reason}")
            }
            (Self::PartialDir, target) => format!(
                "Unable to create partial-dir for {} -- discarding completed file.",
                target.map_or_else(String::new, |t| t.display().to_string())
            ),
        }
    }
}

/// Payload attached to a failed commit-path operation so the caller can name
/// both the operation and the path it acted on.
///
/// Upstream reports the operand of the failing call, not the final
/// destination: `rsyserr(FERROR_XFER, errno, "mkstemp %s failed",
/// full_fname(fnametmp))` (`rsync-3.5.1/receiver.c:465-466`). Both receivers
/// hand failures back as a plain [`io::Error`], so the operation and its
/// operand ride along inside it and are recovered with [`commit_op_failure`].
///
/// [`fmt::Display`] forwards to the underlying error verbatim, so a wrapped
/// error still renders exactly like the unwrapped one.
#[derive(Debug)]
struct CommitOpError {
    op: CommitOp,
    operand: PathBuf,
    destination: Option<PathBuf>,
    source: io::Error,
}

impl fmt::Display for CommitOpError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.source, f)
    }
}

impl std::error::Error for CommitOpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Returns the commit-path operation an error came from, with the path that
/// operation acted on.
///
/// `None` for an error raised outside the annotated stages, so callers fall
/// back to whatever name they already have.
pub fn commit_op_failure(error: &io::Error) -> Option<(CommitOp, &Path)> {
    error
        .get_ref()?
        .downcast_ref::<CommitOpError>()
        .map(|e| (e.op, e.operand.as_path()))
}

/// Returns the second path the failing operation named, for the operations
/// whose upstream diagnostic prints two operands.
///
/// upstream `backup.c:402-403` reports `keep_backup failed: %s -> "%s"` -
/// `full_fname(fname)` and the backup destination `buf`. Only the code that
/// chose that destination knows it, so it is attached at the raising site
/// rather than reconstructed by the reporter. `None` whenever the operation
/// has a single operand, so callers render the one-operand form.
pub fn commit_op_destination(error: &io::Error) -> Option<&Path> {
    error
        .get_ref()?
        .downcast_ref::<CommitOpError>()?
        .destination
        .as_deref()
}

/// Tags `source` with the operation that raised it, preserving its
/// [`io::ErrorKind`] so existing predicates keep classifying it unchanged.
///
/// An error an inner stage already tagged keeps that tag: the innermost stage
/// is the one upstream names, so a failed copy fallback stays a `copy` failure
/// when the rename that chose it wraps the error on the way out.
pub fn attach_commit_op(op: CommitOp, operand: &Path, source: io::Error) -> io::Error {
    attach_commit_op_between(op, operand, None, source)
}

/// [`attach_commit_op`] for an operation that moved `operand` to
/// `destination`, so the reporter can print both halves the way upstream does.
pub fn attach_commit_op_between(
    op: CommitOp,
    operand: &Path,
    destination: Option<&Path>,
    source: io::Error,
) -> io::Error {
    if commit_op_failure(&source).is_some() {
        return source;
    }
    io::Error::new(
        source.kind(),
        CommitOpError {
            op,
            operand: operand.to_path_buf(),
            destination: destination.map(Path::to_path_buf),
            source,
        },
    )
}

/// How a receiver answers a failed commit, decided by the operation that
/// failed - never by the errno. The one place the split is made, shared by the
/// network receiver and the local-copy executor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitDisposition {
    /// Report an `FERROR_XFER` line, skip the file and keep going (exit 23).
    SkipFile,
    /// Report the line with this code, then stop with `RERR_FILEIO` (11).
    Abort(MessageCode),
    /// Report a plain `FERROR` line and drop the file, which is not a transfer
    /// error: the run's exit code is unchanged.
    Discard,
    /// Not a commit-path operation: hand the error back untouched.
    Propagate,
}

impl CommitDisposition {
    /// Classifies `error` by the [`CommitOp`] its
    /// raising site attached.
    ///
    /// # Upstream Reference
    ///
    /// - `receiver.c:465-468` - `mkstemp %s failed`, `return -1`: per-file.
    /// - `receiver.c:1242-1261` - `open %s failed`, then
    ///   `discard_receive_data()` and `continue`: per-file.
    /// - `rsync.c:918-927` - `rename %s -> "%s"`, `finish_transfer()` returns
    ///   0 and `recv_files()` moves on: per-file.
    /// - `rsync.c:897-900` - a failed backup is `exit_cleanup(RERR_FILEIO)`.
    /// - `receiver.c:1296-1300` - a failed `close()` is
    ///   `rsyserr(FERROR, ...)` then `exit_cleanup(RERR_FILEIO)`.
    /// - `receiver.c:1319-1325` - a failed staging-dir create is
    ///   `rprintf(FERROR, ...)`, `do_unlink_at(fnametmp)` and `recv_ok = -1`;
    ///   plain FERROR never sets `got_xfer_error`, so the run can still exit 0.
    #[must_use]
    pub fn of(error: &io::Error) -> Self {
        match commit_op_failure(error) {
            Some((CommitOp::Mkstemp | CommitOp::Open | CommitOp::Rename | CommitOp::Copy, _)) => {
                Self::SkipFile
            }
            Some((CommitOp::Backup, _)) => Self::Abort(MessageCode::ErrorXfer),
            Some((CommitOp::Close, _)) => Self::Abort(MessageCode::Error),
            Some((CommitOp::PartialDir, _)) => Self::Discard,
            None => Self::Propagate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each operation renders upstream's own wording; a copy fallback says
    /// `copy`, not `rename` (rsync.c:919-921, `ret == -2`).
    #[test]
    fn failure_line_mirrors_each_upstream_rsyserr() {
        let name = "\"/d/.a.XXXXXX\"";
        let target = Some(Path::new("a"));
        let reason = "Permission denied (13)";
        let cases = [
            (CommitOp::Mkstemp, None, "mkstemp \"/d/.a.XXXXXX\" failed"),
            (CommitOp::Open, None, "open \"/d/.a.XXXXXX\" failed"),
            (CommitOp::Close, None, "close failed on \"/d/.a.XXXXXX\""),
            (CommitOp::Rename, target, "rename \"/d/.a.XXXXXX\" -> \"a\""),
            (CommitOp::Copy, target, "copy \"/d/.a.XXXXXX\" -> \"a\""),
            (
                CommitOp::Backup,
                target,
                "keep_backup failed: \"/d/.a.XXXXXX\" -> \"a\"",
            ),
            (
                CommitOp::Backup,
                None,
                "keep_backup failed: \"/d/.a.XXXXXX\"",
            ),
        ];
        for (op, target, body) in cases {
            assert_eq!(
                op.failure_line(name, target, reason),
                format!("rsync: [receiver] {body}: {reason}")
            );
        }
        assert_eq!(
            CommitOp::PartialDir.failure_line(name, Some(Path::new("sub/a")), reason),
            "Unable to create partial-dir for sub/a -- discarding completed file."
        );
    }

    /// A staging-dir failure discards the file without a transfer error; every
    /// other commit stage is skip-or-abort.
    ///
    /// upstream: receiver.c:1319-1325.
    #[test]
    fn a_partial_dir_failure_discards_without_a_transfer_error() {
        let tagged = |op| {
            attach_commit_op(
                op,
                Path::new("/d/.~tmp~"),
                io::Error::from(io::ErrorKind::PermissionDenied),
            )
        };
        assert_eq!(
            CommitDisposition::of(&tagged(CommitOp::PartialDir)),
            CommitDisposition::Discard
        );
        assert_eq!(
            CommitDisposition::of(&tagged(CommitOp::Rename)),
            CommitDisposition::SkipFile
        );
    }

    /// The innermost stage names the failure: a copy fallback wrapped by the
    /// rename that chose it is still a copy.
    #[test]
    fn an_inner_tag_survives_an_outer_one() {
        let inner = attach_commit_op(
            CommitOp::Copy,
            Path::new("/t/.a"),
            io::Error::from(io::ErrorKind::StorageFull),
        );
        let outer = attach_commit_op(CommitOp::Rename, Path::new("/elsewhere"), inner);
        assert_eq!(
            commit_op_failure(&outer),
            Some((CommitOp::Copy, Path::new("/t/.a")))
        );
        assert_eq!(outer.kind(), io::ErrorKind::StorageFull);
    }
}
