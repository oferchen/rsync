//! What a server reports for a failure that ends its transfer: the cause
//! upstream prints with `rprintf(FERROR, ...)`, then log_exit()'s trailer.

use std::io;

use crate::branding::Brand;

use super::ExitCode;
use crate::message::Role;

/// The exit code of a failure upstream reports as `rprintf(FERROR, cause)`
/// followed by `exit_cleanup(code)`, or `None` for any other failure.
///
/// upstream: xattrs.c:1253-1258 get_stat_xattr() (RERR_FILEIO),
/// util2.c:73-80 my_alloc() (RERR_MALLOC)
#[must_use]
pub fn cause_line_exit_code(error: &io::Error) -> Option<ExitCode> {
    if metadata::is_corrupt_fake_super(error) {
        Some(ExitCode::FileIo)
    } else if error
        .get_ref()
        .is_some_and(|inner| inner.is::<protocol::MallocFailure>())
    {
        Some(ExitCode::Malloc)
    } else {
        None
    }
}

/// The newline-terminated lines a server reports when `error` ends its
/// transfer with `code`: the failure's own cause, when upstream prints one,
/// then the `rsync error: ... (code N) at ... [role=version]` trailer.
///
/// upstream: log.c:937-963 log_exit()
#[must_use]
pub fn fatal_report_lines(
    error: &io::Error,
    code: ExitCode,
    role: Role,
    brand: Brand,
) -> Vec<Vec<u8>> {
    let mut lines = Vec::with_capacity(2);
    if cause_line_exit_code(error).is_some() {
        lines.push(format!("{error}\n").into_bytes());
    }
    let trailer = crate::rsync_error!(code.as_i32(), "{}", code.description())
        .with_role(role)
        .with_brand(brand);
    if let Ok(line) = trailer.to_line_bytes() {
        lines.push(line);
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A failure upstream prints a cause for is reported with that cause
    /// before the trailer; any other failure gets the trailer alone.
    #[test]
    fn the_cause_precedes_the_trailer_only_when_upstream_prints_one() {
        let refused = protocol::malloc_failure("[sender] exceeded --max-alloc=1048576 setting");
        let lines = fatal_report_lines(&refused, ExitCode::Malloc, Role::Sender, Brand::Upstream);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], b"[sender] exceeded --max-alloc=1048576 setting\n");
        let trailer = String::from_utf8(lines[1].clone()).expect("utf-8");
        assert!(
            trailer.starts_with("rsync error: error allocating core memory buffers (code 22) at "),
            "{trailer}"
        );
        assert!(trailer.contains("[sender="), "{trailer}");
        assert!(trailer.ends_with('\n'));

        let plain = io::Error::other("boom");
        let lines = fatal_report_lines(
            &plain,
            ExitCode::PartialTransfer,
            Role::Sender,
            Brand::Upstream,
        );
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with(b"rsync error: "));
    }
}
