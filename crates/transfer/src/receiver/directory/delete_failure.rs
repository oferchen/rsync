//! Classification and reporting of a failed `--delete` removal.
//!
//! Every delete arm - the parallel scan, the capped serial executor, the
//! `--delete-delay` replay and the obstacle removal - reports a removal that
//! failed through [`ReceiverContext::report_delete_failure`], so each prints
//! exactly what upstream's `delete_item()` prints for the same errno.
//!
//! # Upstream Reference
//!
//! - `delete.c:225-236` - `delete_item()` names the failing step `rmdir`,
//!   `make_backup` or `unlink`.
//! - `delete.c:260-269` - `ENOTEMPTY` on a directory is a `FINFO` notice,
//!   `ENOENT` counts as success, and every other errno is
//!   `rsyserr(FERROR_XFER, errno, "delete_file: %s(%s) failed", what, fbuf)`.

use std::io;
use std::path::Path;

use crate::receiver::ReceiverContext;

/// The removal step that failed, named as `delete_item()` names it.
///
/// upstream: delete.c:225-236 - `what = "rmdir"`, `"make_backup"` or
/// `"unlink"`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::receiver) enum DeleteStep {
    /// Removing an emptied directory.
    Rmdir,
    /// Moving a victim into the backup area under `--backup`.
    MakeBackup,
    /// Unlinking a non-directory.
    Unlink,
}

impl DeleteStep {
    /// The word upstream puts before the name in its `delete_file:` line.
    const fn what(self) -> &'static str {
        match self {
            Self::Rmdir => "rmdir",
            Self::MakeBackup => "make_backup",
            Self::Unlink => "unlink",
        }
    }
}

/// A removal that returned an error, with the step that returned it.
#[derive(Debug)]
pub(in crate::receiver) struct DeleteFailure {
    /// The step that failed.
    pub(in crate::receiver) step: DeleteStep,
    /// The error that step returned.
    pub(in crate::receiver) error: io::Error,
}

impl DeleteFailure {
    /// Tags `error` with the step that returned it.
    pub(in crate::receiver) const fn new(step: DeleteStep, error: io::Error) -> Self {
        Self { step, error }
    }
}

impl From<DeleteFailure> for io::Error {
    fn from(failure: DeleteFailure) -> Self {
        failure.error
    }
}

/// What a failed removal means for the delete pass.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::receiver) enum DeleteVerdict {
    /// The entry was already gone: upstream counts `ENOENT` as success.
    Vanished,
    /// A directory still holds entries; reported at `FINFO` and kept.
    NotEmpty,
    /// Any other errno; reported at `FERROR_XFER`, which makes the run
    /// exit `RERR_PARTIAL` (23).
    Failed,
}

/// Classifies `failure` the way `delete_item()` does.
///
/// upstream: delete.c:260-269.
pub(in crate::receiver) fn classify(failure: &DeleteFailure) -> DeleteVerdict {
    if failure.error.kind() == io::ErrorKind::NotFound {
        DeleteVerdict::Vanished
    } else if failure.step == DeleteStep::Rmdir && super::obstacle::is_not_empty(&failure.error) {
        DeleteVerdict::NotEmpty
    } else {
        DeleteVerdict::Failed
    }
}

/// Renders a deletion-root-relative name as upstream prints `fbuf`.
fn display_name(rel: &Path) -> String {
    rel.display().to_string().replace('\\', "/")
}

impl ReceiverContext {
    /// Reports a removal that failed, as `delete_item()` reports it, and
    /// returns the verdict.
    ///
    /// The `FERROR_XFER` line sets `got_xfer_error`, so a failed removal can
    /// never end in exit 0. A failed write of the notice itself is ignored:
    /// the delete pass goes on to the next entry either way, as upstream's
    /// does.
    ///
    /// # Upstream Reference
    ///
    /// - `delete.c:260-263` - `cannot delete non-empty directory: %s` at `FINFO`
    /// - `delete.c:264-267` - `delete_file: %s(%s) failed` at `FERROR_XFER`
    /// - `log.c:390-391` - `FERROR_XFER` sets `got_xfer_error`
    pub(in crate::receiver) fn report_delete_failure<W>(
        &self,
        writer: &mut W,
        rel: &Path,
        failure: &DeleteFailure,
    ) -> DeleteVerdict
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let verdict = classify(failure);
        match verdict {
            DeleteVerdict::Vanished => {}
            DeleteVerdict::NotEmpty => self.report_non_empty_dir(writer, rel),
            DeleteVerdict::Failed => {
                let _ = self.emit_generator_error_xfer(
                    writer,
                    &format!(
                        "delete_file: {}({}) failed",
                        failure.step.what(),
                        display_name(rel)
                    ),
                    &failure.error,
                );
            }
        }
        verdict
    }

    /// Prints the notice for a directory the delete pass had to keep.
    ///
    /// upstream: delete.c:178-180 and :260-263 - `rprintf(FINFO, "cannot
    /// delete non-empty directory: %s\n", ...)`.
    pub(in crate::receiver) fn report_non_empty_dir<W>(&self, writer: &mut W, rel: &Path)
    where
        W: crate::writer::MsgInfoSender + ?Sized,
    {
        let _ = self.emit_info_line(
            writer,
            &format!("cannot delete non-empty directory: {}\n", display_name(rel)),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn failure(step: DeleteStep, errno: i32) -> DeleteFailure {
        DeleteFailure::new(step, io::Error::from_raw_os_error(errno))
    }

    /// upstream: delete.c:268-269 - a victim that vanished before the
    /// removal is a success, whatever the step.
    #[test]
    fn a_vanished_victim_is_not_a_failure() {
        for step in [
            DeleteStep::Rmdir,
            DeleteStep::MakeBackup,
            DeleteStep::Unlink,
        ] {
            assert_eq!(
                classify(&DeleteFailure::new(
                    step,
                    io::Error::from(io::ErrorKind::NotFound)
                )),
                DeleteVerdict::Vanished
            );
        }
    }

    /// upstream: delete.c:260 - only a directory's `ENOTEMPTY` is the
    /// non-empty notice; the same errno from any other step is a failure.
    #[cfg(unix)]
    #[test]
    fn only_a_directory_is_reported_as_non_empty() {
        assert_eq!(
            classify(&failure(DeleteStep::Rmdir, fast_io::errno::ENOTEMPTY)),
            DeleteVerdict::NotEmpty
        );
        assert_eq!(
            classify(&failure(DeleteStep::Unlink, fast_io::errno::ENOTEMPTY)),
            DeleteVerdict::Failed
        );
    }

    /// upstream: delete.c:264-267 - every other errno is a failure that
    /// lifts the exit code, including the confinement refusals.
    #[cfg(unix)]
    #[test]
    fn every_other_errno_is_a_failure() {
        for errno in [
            fast_io::errno::EACCES,
            fast_io::errno::EPERM,
            fast_io::errno::EBUSY,
            fast_io::errno::ELOOP,
            fast_io::errno::ENOTDIR,
        ] {
            assert_eq!(
                classify(&failure(DeleteStep::Unlink, errno)),
                DeleteVerdict::Failed,
                "errno {errno}"
            );
        }
    }

    #[cfg(unix)]
    /// Records each frame a server receiver sends, with its channel.
    #[derive(Default)]
    struct FrameCapture(Vec<(&'static str, String)>);

    #[cfg(unix)]
    impl std::io::Write for FrameCapture {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    impl crate::writer::MsgInfoSender for FrameCapture {
        fn send_msg_info(&mut self, data: &[u8]) -> io::Result<()> {
            self.0
                .push(("FINFO", String::from_utf8_lossy(data).into_owned()));
            Ok(())
        }
        fn send_msg_error_xfer(&mut self, data: &[u8]) -> io::Result<()> {
            self.0
                .push(("FERROR_XFER", String::from_utf8_lossy(data).into_owned()));
            Ok(())
        }
    }

    #[cfg(unix)]
    fn server_receiver() -> ReceiverContext {
        let handshake = crate::handshake::HandshakeResult {
            protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
            buffered: Vec::new(),
            compat_exchanged: false,
            client_args: None,
            io_timeout: None,
            negotiated_algorithms: None,
            compat_flags: None,
            checksum_seed: 0,
        };
        let config = crate::config::ServerConfig {
            role: crate::role::ServerRole::Receiver,
            protocol: protocol::ProtocolVersion::try_from(32u8).unwrap(),
            ..Default::default()
        };
        ReceiverContext::new_for_test(&handshake, config)
    }

    /// The three outcomes go out on upstream's channels, and only the real
    /// failure lifts the exit code: a server receiver relays the FERROR_XFER
    /// line and still records `got_xfer_error` locally (log.c:390-391), so a
    /// failed delete can never end in exit 0.
    #[cfg(unix)]
    #[test]
    fn the_reporter_writes_delete_items_lines_and_flags_only_a_failure() {
        let ctx = server_receiver();
        let mut writer = FrameCapture::default();

        let vanished =
            DeleteFailure::new(DeleteStep::Unlink, io::Error::from(io::ErrorKind::NotFound));
        assert_eq!(
            ctx.report_delete_failure(&mut writer, Path::new("gone"), &vanished),
            DeleteVerdict::Vanished
        );
        assert!(writer.0.is_empty(), "a vanished victim prints nothing");

        let kept = failure(DeleteStep::Rmdir, fast_io::errno::ENOTEMPTY);
        assert_eq!(
            ctx.report_delete_failure(&mut writer, Path::new("history"), &kept),
            DeleteVerdict::NotEmpty
        );
        assert!(
            !ctx.got_xfer_error.get(),
            "the non-empty notice is FINFO and must not lift the exit code"
        );

        let busy = failure(DeleteStep::Rmdir, fast_io::errno::EBUSY);
        assert_eq!(
            ctx.report_delete_failure(&mut writer, Path::new("a/mnt"), &busy),
            DeleteVerdict::Failed
        );
        let refused = failure(DeleteStep::MakeBackup, fast_io::errno::EACCES);
        ctx.report_delete_failure(&mut writer, Path::new("a/f"), &refused);

        assert_eq!(
            writer.0,
            [
                (
                    "FINFO",
                    "cannot delete non-empty directory: history\n".to_owned()
                ),
                (
                    "FERROR_XFER",
                    format!(
                        "rsync: [generator] delete_file: rmdir(a/mnt) failed: {}\n",
                        logging::upstream_errno_text(&busy.error)
                    ),
                ),
                (
                    "FERROR_XFER",
                    format!(
                        "rsync: [generator] delete_file: make_backup(a/f) failed: {}\n",
                        logging::upstream_errno_text(&refused.error)
                    ),
                ),
            ]
        );
        assert!(
            ctx.got_xfer_error.get(),
            "a failed removal must make the run exit RERR_PARTIAL (23)"
        );
    }

    #[test]
    fn each_step_is_named_as_delete_item_names_it() {
        assert_eq!(DeleteStep::Rmdir.what(), "rmdir");
        assert_eq!(DeleteStep::MakeBackup.what(), "make_backup");
        assert_eq!(DeleteStep::Unlink.what(), "unlink");
    }
}
